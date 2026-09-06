//! FireService — the one pipeline from prompt id to typed output, shared by
//! the hook, the picker, the tray and per-prompt hotkeys.
//!
//! Numbered stage comments below follow: capture foreground → pick candidate →
//! focused-element guard → gather referenced context → expressions →
//! placeholders → filters → case → RDP → newline mode → schedule → play →
//! undo and usage.

use crate::accessibility;
use crate::app::context::AppContext;
use crate::filters;
use crate::inject::{paste_via_clipboard, EnigoInjector};
use crate::matcher;
use crate::prompts::expressions::ExprContext;
use crate::prompts::placeholders::{expand, PlaceholderContext};
use crate::prompts::steps::{self, Step};
use crate::prompts::Prompt;
use crate::rdp::RdpMode;
use crate::repo;
use crate::scopes;
use crate::telemetry::{
    self, CancelReason, CharBucket, DurationBucket, InjectionStage, PromptMode, TargetAppKind,
    TelemetryEvent,
};
use crate::typer::{play_controlled, schedule, Injector, Key, PlaybackControl, ScheduleOptions};
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::Instant;
use tauri::AppHandle;

/// Pickup mode used when firing from the picker (Spec §5.3 modifier-on-Enter).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickMode {
    Human,
    Fast,
    Paste,
    /// §5.3 Cmd+Enter — type the body, then submit it. The one mode the spec
    /// listed and the picker never had; it is also what makes this useful
    /// against a coding agent, where "typed but not sent" is half a turn.
    Run,
}

impl PickMode {
    pub fn parse(s: &str) -> Self {
        match s {
            "fast" => Self::Fast,
            "paste" => Self::Paste,
            "run" => Self::Run,
            _ => Self::Human,
        }
    }

    pub fn is_paste(&self) -> bool {
        matches!(self, Self::Paste)
    }
}

/// Everything one fire needs beyond shared app state. Bundled so the pipeline
/// takes four arguments instead of a dozen positional ones.
struct FireRequest {
    prompt: Prompt,
    typed_form: Option<String>,
    telem_mode: PromptMode,
    foreground: scopes::ForegroundContext,
    pick_mode: Option<PickMode>,
    /// Pre-resolved tab-stop / choice answers from the picker (§6.4: the
    /// picker resolves choices before it dismisses, never a modal mid-typing).
    stop_answers: HashMap<String, String>,
}

#[derive(Clone)]
pub struct FireService {
    ctx: AppContext,
    app: AppHandle,
}

impl FireService {
    pub fn new(ctx: AppContext, app: AppHandle) -> Self {
        Self { ctx, app }
    }

    /// Fire from the hook. Only spawns a thread — this runs under Windows'
    /// ~300ms `LowLevelHooksTimeout`, and overrunning it unhooks us.
    pub fn fire_from_trigger(&self, candidate_ids: Vec<String>, typed_form: String) {
        let svc = self.clone();
        thread::Builder::new()
            .name("prompt-player-fire-stealth".into())
            .spawn(move || {
                let foreground = scopes::capture_foreground_context();
                let candidates: Vec<Prompt> = {
                    let all = svc.ctx.prompts.read();
                    candidate_ids
                        .iter()
                        .filter_map(|id| all.iter().find(|p| &p.id == id).cloned())
                        .filter(|p| p.enabled)
                        .collect()
                };
                if candidates.is_empty() {
                    tracing::debug!("trigger matched, but no enabled candidate; nothing fires");
                    return;
                }
                let Some(picked_id) = scopes::pick_best(&candidates, &foreground) else {
                    // Matched but no scope fits: put the eaten commit char back
                    // and report, since this looks like a broken trigger.
                    telemetry::send(
                        &svc.app,
                        TelemetryEvent::ScopeRejected {
                            candidates: crate::telemetry::CountBucket::classify(candidates.len()),
                            target_app_kind: classify_target_app(
                                &foreground,
                                svc.ctx.rdp.detect(&foreground),
                            ),
                        },
                    );
                    let commit = candidates[0].commit_char;
                    tracing::debug!("no scope match for trigger; re-injecting '{}'", commit);
                    if let Ok(mut inj) = EnigoInjector::new() {
                        inj.type_char(commit);
                    }
                    let now = std::time::Instant::now();
                    for ch in typed_form.chars() {
                        svc.ctx.matcher.observe_char(ch, now);
                    }
                    svc.ctx.matcher.observe_char(commit, now);
                    return;
                };
                let Some(prompt) = candidates.into_iter().find(|p| p.id == picked_id) else {
                    tracing::warn!("picked prompt {} not found", picked_id);
                    return;
                };
                svc.run_resolved(FireRequest {
                    prompt,
                    typed_form: Some(typed_form),
                    telem_mode: PromptMode::Stealth,
                    foreground,
                    pick_mode: None,
                    stop_answers: HashMap::new(),
                });
            })
            .expect("spawn fire dispatch thread");
    }

    /// Fire from the picker. `mode` is the modifier-on-Enter behavior; the
    /// pipeline (scope, expressions, filters) is otherwise identical.
    pub fn fire_from_picker(&self, prompt_id: &str, mode: PickMode) {
        self.fire_selected(prompt_id, mode, PromptMode::Picker, HashMap::new());
    }

    /// Picker fire carrying pre-resolved tab-stop / choice answers.
    pub fn fire_from_picker_with(
        &self,
        prompt_id: &str,
        mode: PickMode,
        stop_answers: HashMap<String, String>,
    ) {
        self.fire_selected(prompt_id, mode, PromptMode::Picker, stop_answers);
    }

    /// Fire a pinned prompt clicked in the tray. Same pipeline as the picker;
    /// only the reported mode differs, so tray use is finally distinguishable.
    pub fn fire_from_tray(&self, prompt_id: &str, mode: PickMode) {
        self.fire_selected(prompt_id, mode, PromptMode::Tray, HashMap::new());
    }

    fn fire_selected(
        &self,
        prompt_id: &str,
        mode: PickMode,
        telem_mode: PromptMode,
        stop_answers: HashMap<String, String>,
    ) {
        let svc = self.clone();
        let prompt_id = prompt_id.to_string();
        thread::Builder::new()
            .name("prompt-player-fire-picker".into())
            .spawn(move || {
                let Some(prompt) = svc.ctx.prompts.find(&prompt_id) else {
                    tracing::warn!("picker selected unknown prompt {}", prompt_id);
                    return;
                };
                if !prompt.enabled {
                    tracing::info!("picker selected disabled prompt {}; ignoring", prompt_id);
                    return;
                }
                // Picker runs after focus restore — capture foreground anew so
                // scope/expr context reflects the target app, not Prompt Player.
                let foreground = scopes::capture_foreground_context();
                svc.run_resolved(FireRequest {
                    prompt,
                    typed_form: None,
                    telem_mode,
                    foreground,
                    pick_mode: Some(mode),
                    stop_answers,
                });
            })
            .expect("spawn picker fire thread");
    }

    /// Fire from a per-prompt global hotkey. No commit-char, no typed form.
    pub fn fire_from_hotkey(&self, prompt_id: &str) {
        let svc = self.clone();
        let prompt_id = prompt_id.to_string();
        thread::Builder::new()
            .name("prompt-player-fire-hotkey".into())
            .spawn(move || {
                let Some(prompt) = svc.ctx.prompts.find(&prompt_id) else {
                    return;
                };
                if !prompt.enabled {
                    return;
                }
                let foreground = scopes::capture_foreground_context();
                telemetry::send(&svc.app, TelemetryEvent::HotkeyFired);
                svc.run_resolved(FireRequest {
                    prompt,
                    typed_form: None,
                    telem_mode: PromptMode::Hotkey,
                    foreground,
                    pick_mode: None,
                    stop_answers: HashMap::new(),
                });
            })
            .expect("spawn hotkey fire thread");
    }

    /// Run a resolved fire on the dispatch thread. Bails if a playback is
    /// already typing, so two fires can't interleave into one window.
    fn run_resolved(&self, req: FireRequest) {
        let Some(control) = self.ctx.state.begin_playback() else {
            tracing::info!("fire ignored — a playback is already in progress");
            return;
        };
        run_fire_pipeline(self.app.clone(), self.ctx.clone(), req, control);
    }

    /// Backspace-undo. Only the body is erased — the trigger was never removed
    /// — then it's re-observed so `trigger>` can fire again right away.
    pub fn run_undo(&self) {
        let app_state = self.ctx.state.clone();
        let undo = self.ctx.undo.clone();
        let matcher = self.ctx.matcher.clone();
        let app = self.app.clone();
        thread::Builder::new()
            .name("prompt-player-undo".into())
            .spawn(move || {
                let Some(entry) = undo.take_recent(std::time::Instant::now()) else {
                    return;
                };
                let Some(control) = app_state.begin_playback() else {
                    // A playback is already running — don't stomp it.
                    return;
                };
                // Same RAII guard the fire path uses. Three hand-written
                // `end_playback()` calls were correct, but one early return
                // away from leaving `playing` true forever — which silently
                // disables every trigger for the rest of the session.
                let _playback_guard = PlaybackEndGuard::new(app_state.clone());
                let mut inj = match EnigoInjector::new() {
                    Ok(i) => i,
                    Err(e) => {
                        tracing::error!("undo enigo init failed: {:?}", e);
                        telemetry::send(
                            &app,
                            TelemetryEvent::InjectionFailed {
                                stage: InjectionStage::Undo,
                            },
                        );
                        return;
                    }
                };
                for _ in 0..entry.body_chars_typed {
                    if control.is_cancelled() {
                        inj.release_all_modifiers();
                        return;
                    }
                    inj.press_backspace();
                    thread::sleep(std::time::Duration::from_millis(15));
                }
                drop(_playback_guard);
                // Re-sync the ring with the screen: the trigger is still visible
                // but was popped at fire time.
                let now = std::time::Instant::now();
                for c in entry.trigger_form.chars() {
                    matcher.observe_char(c, now);
                }
                telemetry::send(&app, TelemetryEvent::PromptUndone);
            })
            .expect("spawn undo thread");
    }
}

fn run_fire_pipeline(app: AppHandle, ctx: AppContext, req: FireRequest, control: PlaybackControl) {
    let FireRequest {
        prompt,
        typed_form,
        telem_mode,
        foreground,
        pick_mode,
        stop_answers,
    } = req;
    let app_state = ctx.state.clone();
    let _playback_guard = PlaybackEndGuard::new(app_state.clone());
    let config = ctx.config.get();

    // 3 — §11 focused-element pre-flight. The one check that stops a prompt
    // landing in a password box, a Finder window, or a file dialog. Only the
    // two confident negatives block; "unknown" proceeds (see
    // `accessibility::FieldKind`), because a guard that blocked Electron and
    // terminal surfaces would break the app's primary targets.
    if config.text_field_guard {
        let field = accessibility::focused_field();
        if !field.kind.allows_typing() {
            tracing::warn!(
                target: "prompt_player::fire",
                role = field.role.as_deref().unwrap_or("<unknown>"),
                verdict = field.kind.reason(),
                "refusing to type: focused element cannot accept text"
            );
            telemetry::send(
                &app,
                TelemetryEvent::TypingBlocked {
                    reason: field.kind.reason(),
                },
            );
            // On the trigger path the hook has already swallowed the commit
            // char and popped the trigger from the ring. Put both back, the
            // same way the no-scope-match path does — otherwise a refusal
            // leaves the user with a silently missing `>` and a matcher whose
            // shadow of the screen is wrong for the next attempt.
            if let Some(form) = typed_form.as_deref() {
                restore_suppressed_commit(&ctx, form, prompt.commit_char);
            }
            return;
        }
    }

    // 4 — gather only the context this body references. Each of these costs
    // something real (a clipboard read, an Accessibility round-trip, a
    // filesystem walk), so an unrelated prompt must not pay for it.
    let body_src = prompt.body.as_str();
    // Match the tokens, not the bare word: `contains("clipboard")` meant a
    // prompt whose prose merely mentions the clipboard read the user's
    // clipboard on every fire.
    let clipboard = if references_clipboard(body_src) {
        crate::inject::read_clipboard_text()
    } else {
        None
    };
    // §6.1 `$SELECTION` — declared in the spec, never populated until now, so
    // every prompt referencing it (including the shipped refactor example)
    // expanded to an empty string.
    let selection = if body_src.contains("SELECTION") || body_src.contains("selection") {
        accessibility::selected_text()
    } else {
        None
    };
    let wants_repo = ["GIT_BRANCH", "REPO_NAME", "REPO_ROOT", "CWD", "repo."]
        .iter()
        .any(|needle| body_src.contains(needle));
    let repo_ctx = if wants_repo {
        repo::resolve(foreground.window_title.as_deref(), &config.repo_hints)
    } else {
        repo::RepoContext::default()
    };

    // 5 — TS expressions.
    let app_name = foreground
        .executable
        .as_deref()
        .and_then(|s| std::path::Path::new(s).file_name().and_then(|f| f.to_str()))
        .map(|s| s.to_string());
    let expr_ctx = ExprContext {
        app_bundle: foreground.bundle_id.clone(),
        app_name,
        window_title: foreground.window_title.clone(),
        clipboard: clipboard.clone(),
        selection: selection.clone(),
        git_branch: repo_ctx.branch.clone(),
        repo_name: repo_ctx.name.clone(),
        repo_root: repo_ctx.root.clone(),
        // A remote repository does not get to run commands on this machine,
        // whatever the config says.
        allow_git: config.allow_git_expressions && !prompt.origin.is_remote(),
    };
    let expansion =
        crate::prompts::expressions::expand_expressions_reporting(&prompt.body, &expr_ctx);
    let has_expressions = expansion.had_expressions;
    for err in &expansion.errors {
        telemetry::send(&app, TelemetryEvent::ExpressionError { kind: err.kind() });
    }
    let body_after_expr = expansion.text;

    // 6 — placeholders.
    let ph_ctx = PlaceholderContext {
        app_bundle: foreground.bundle_id.clone(),
        app_name: expr_ctx.app_name.clone(),
        window_title: foreground.window_title.clone(),
        clipboard,
        selection,
        git_branch: repo_ctx.branch.clone(),
        repo_name: repo_ctx.name.clone(),
        repo_root: repo_ctx.root.clone(),
        stop_answers,
        ..Default::default()
    };
    let expanded = expand(&body_after_expr, &ph_ctx);
    if !expanded.unfilled_stops.is_empty() {
        // Not fatal: an unanswered stop renders as its default (or empty) and
        // the user fills it in live, which is the §6.4 "scaffold + live detail"
        // pattern. Worth a log line so a silently-empty body has an explanation.
        tracing::info!(
            target: "prompt_player::fire",
            stops = ?expanded.unfilled_stops,
            "firing with unresolved tab stops"
        );
    }

    // 5 — filter chain.
    let filtered = filters::apply_chain(&expanded.text, &prompt.filters);

    // 6 — case propagation (trigger path only).
    let body = match &typed_form {
        Some(form) => matcher::propagate_case(form, &filtered),
        None => filtered,
    };

    // 7 — RDP detection.
    let rdp_mode = ctx.rdp.detect(&foreground);
    if rdp_mode == RdpMode::HostSide {
        tracing::info!(
            "rdp host-side mode active for {:?}",
            foreground
                .bundle_id
                .as_deref()
                .or(foreground.executable.as_deref())
        );
        telemetry::send(&app, TelemetryEvent::RdpDetected);
    }

    // Picker pick_mode customization.
    let mut profile = prompt.effective_profile();
    // 9 — how a newline is delivered depends on the target surface, and
    // getting it wrong submits the prompt at its first blank line. Per-prompt
    // `newline-mode:` wins over the library default.
    let newline_mode = prompt.newline_mode.unwrap_or(config.newline_mode);
    let mut opts = ScheduleOptions {
        rdp_mode: rdp_mode == RdpMode::HostSide,
        // Only meaningful after a suppressed `>`; picker selections type into
        // an already-restored app.
        include_pre_typing_pause: typed_form.is_some(),
        newline_mode,
    };
    let mut paste_mode = false;
    if let Some(mode) = pick_mode {
        opts.include_pre_typing_pause = false;
        match mode {
            PickMode::Fast => {
                // More aggressive than FAST_PRESENTER: still typed, but now.
                // Low `iki_min_ms` keeps the global floor from clamping the scale.
                profile.iki_scale = 0.16;
                profile.iki_min_ms = 12.0;
                profile.pause_scale = 0.2;
                profile.pause_variance_scale = 0.3;
                profile.typos_enabled = false;
                profile.pre_submit_pause_enabled = false;
                profile.burst_enabled = false;
            }
            PickMode::Paste => paste_mode = true,
            PickMode::Run => {
                // Type at human cadence, then submit. The pre-submit pause is
                // §3.1's "single most realism-defining touch" and matters most
                // here, because this is the mode where an Enter follows.
                profile.send_final_enter = true;
                profile.pre_submit_pause_enabled = true;
            }
            _ => {}
        }
    }
    // §9.3 — RDP clipboard sync is unreliable and can land the body on the
    // wrong side, so paste falls back to typing.
    if paste_mode && rdp_mode == RdpMode::HostSide {
        tracing::info!("paste demoted to typed flow under RDP host-side mode");
        paste_mode = false;
    }
    // A multi-step sequence is several messages with waits between them; one
    // clipboard blob cannot be that, so paste is demoted here too.
    let sequence: Vec<Step> = steps::split_steps(&body);
    let multi_step = sequence.len() > 1;
    if paste_mode && multi_step {
        tracing::info!("paste demoted to typed flow for a multi-step prompt");
        paste_mode = false;
    }

    let target_app = classify_target_app(&foreground, rdp_mode);
    let scoped = prompt.scope.is_some();

    // 8 — schedule. Skipped for paste: one keystroke, no cadence to build,
    // and this avoids seeding the RNG on every paste fire.
    let scheduled = if paste_mode {
        Vec::new()
    } else {
        let mut rng = ChaCha8Rng::from_entropy();
        schedule(&body, &profile, &opts, &mut rng)
    };
    let body_chars: usize = if paste_mode {
        // Paste path: report the body's char count directly. No corrections,
        // no backspaces, no schedule to scan.
        body.chars().count()
    } else {
        scheduled
            .iter()
            .filter(|k| matches!(k.key, Key::Char(_)))
            .count()
            .saturating_sub(
                scheduled
                    .iter()
                    .filter(|k| matches!(k.key, Key::Backspace))
                    .count(),
            )
    };

    let char_bucket = CharBucket::classify(body_chars);
    telemetry::send(
        &app,
        TelemetryEvent::PromptFired {
            mode: telem_mode.clone(),
            char_count_bucket: char_bucket.clone(),
            has_expressions,
            target_app_kind: target_app,
            scoped,
            paste: paste_mode,
        },
    );
    let started_at = std::time::Instant::now();

    // 9 — play. Snapshot the target so we can bail on focus loss: the §2.6
    // panic ring only sees keystrokes, and a silent focus change makes none.
    let target_identity = scopes::foreground_identity();
    let focus_lost = move || match (target_identity, scopes::foreground_identity()) {
        // Abort only when both are known and differ — a transient read failure
        // must never cut off a legitimate playback.
        (Some(t), Some(c)) => t != c,
        _ => false,
    };
    let mut focus_changed = false;
    let mut completed_chars = 0usize;
    let completed = if paste_mode {
        // save → set → Ctrl/Cmd+V → wait → restore. The caller already
        // confirmed focus, and once V is in flight the paste is atomic.
        if control.is_cancelled() {
            false
        } else {
            match paste_via_clipboard(&body) {
                Ok(()) => {
                    completed_chars = body_chars;
                    true
                }
                Err(e) => {
                    tracing::warn!(
                        "clipboard paste failed, falling back to typed playback: {:?}",
                        e
                    );
                    // User-visible: paste becomes per-key typing at a very
                    // different speed. Was tracing-only until now.
                    telemetry::send(
                        &app,
                        TelemetryEvent::InjectionFailed {
                            stage: InjectionStage::Clipboard,
                        },
                    );
                    let mut rng = ChaCha8Rng::from_entropy();
                    let fallback_schedule = schedule(&body, &profile, &opts, &mut rng);
                    match EnigoInjector::new() {
                        Ok(mut inj) => {
                            let _timer = crate::typer::TimerResolutionGuard::acquire();
                            let outcome = play_controlled(
                                &fallback_schedule,
                                &mut inj,
                                &control,
                                Some(&focus_lost),
                            );
                            completed_chars = outcome.visible_chars;
                            focus_changed = outcome.focus_changed;
                            outcome.completed
                        }
                        Err(e) => {
                            tracing::error!("fallback enigo init failed: {:?}", e);
                            telemetry::send(
                                &app,
                                TelemetryEvent::InjectionFailed {
                                    stage: InjectionStage::Init,
                                },
                            );
                            false
                        }
                    }
                }
            }
        }
    } else {
        let mut inj = match EnigoInjector::new() {
            Ok(i) => i,
            Err(e) => {
                tracing::error!("enigo init failed: {:?}", e);
                telemetry::send(
                    &app,
                    TelemetryEvent::InjectionFailed {
                        stage: InjectionStage::Init,
                    },
                );
                telemetry::send(
                    &app,
                    TelemetryEvent::PromptCancelled {
                        reason: CancelReason::Error,
                        completed_chars_pct: 0,
                    },
                );
                return;
            }
        };
        // Hold 1ms timer resolution across the whole typed run so per-key
        // sleeps don't quantize to the OS timer period (Windows-only effect).
        let _timer = crate::typer::TimerResolutionGuard::acquire();
        let outcome = if multi_step {
            play_sequence(
                &sequence,
                &profile,
                &opts,
                &mut inj,
                &control,
                Some(&focus_lost),
            )
        } else {
            play_controlled(&scheduled, &mut inj, &control, Some(&focus_lost))
        };
        completed_chars = outcome.visible_chars;
        focus_changed = outcome.focus_changed;
        drop(inj);
        outcome.completed
    };

    if completed {
        // §5.2 recents tier — a *completed* fire is a use; a cancelled one is
        // not, so this sits inside the success branch.
        ctx.usage.record(&prompt.id);
        let mut undo_offered = false;
        if let Some(form) = typed_form.as_deref() {
            if multi_step {
                // Every step but the last was submitted, so backspacing would
                // delete from whatever field has focus now, not un-send them.
                tracing::debug!("not recording undo: multi-step sequence was submitted");
            } else if profile.send_final_enter {
                // Backspace-undo can't un-send a submitted message, and the
                // backspaces would hit whatever field has focus now.
                tracing::debug!("not recording undo: prompt was submitted via final Enter");
            } else {
                ctx.undo.record(form.to_string(), body_chars);
                undo_offered = true;
            }
        }
        telemetry::send(
            &app,
            TelemetryEvent::PromptCompleted {
                mode: telem_mode,
                char_count_bucket: char_bucket,
                duration: DurationBucket::classify(started_at.elapsed()),
                paste: paste_mode,
                undo_offered,
            },
        );
    } else {
        // Paste is atomic, so `completed=false` there means nothing landed.
        // The typed path estimates from `visible_chars` (coarse, see above).
        let pct = if body_chars == 0 {
            0
        } else {
            (completed_chars * 100 / body_chars).min(100) as u8
        };
        // Focus loss is decided here; everything else was attributed by
        // whoever tripped the flag (Esc, kill-switch, panic ring).
        let reason = if focus_changed {
            CancelReason::FocusChanged
        } else {
            app_state
                .take_cancel_reason()
                .unwrap_or(CancelReason::UserKeystrokes)
        };
        telemetry::send(
            &app,
            TelemetryEvent::PromptCancelled {
                reason,
                completed_chars_pct: pct,
            },
        );
    }
}

/// Undo the hook's commit-char suppression when a fire doesn't happen.
///
/// The hook suppresses the commit char and pops the trigger from its ring the
/// moment a trigger matches — before anything knows whether the fire will
/// actually proceed. Every early return on the trigger path therefore has to
/// put the character back on screen and re-observe the trigger, or the user
/// loses a keystroke and the matcher's shadow of the screen goes stale.
/// Does this body actually reference the clipboard, as opposed to mentioning
/// the word? Reading the clipboard is a privacy-relevant act, so it happens
/// only for the placeholder (`$CLIPBOARD`, `${CLIPBOARD}`) or the expression
/// builtin (`clipboard` inside a `${{ … }}` block).
fn references_clipboard(body: &str) -> bool {
    if body.contains("$CLIPBOARD") || body.contains("${CLIPBOARD}") {
        return true;
    }
    // Expression blocks only — prose outside them can say anything it likes.
    let mut rest = body;
    while let Some(open) = rest.find("${{") {
        let after = &rest[open + 3..];
        let end = after.find("}}").unwrap_or(after.len());
        if after[..end].contains("clipboard") {
            return true;
        }
        rest = &after[end..];
        if rest.len() < 2 {
            break;
        }
        rest = &rest[2..];
    }
    false
}

fn restore_suppressed_commit(ctx: &AppContext, typed_form: &str, commit: char) {
    if let Ok(mut inj) = EnigoInjector::new() {
        inj.type_char(commit);
    }
    let now = std::time::Instant::now();
    for ch in typed_form.chars() {
        ctx.matcher.observe_char(ch, now);
    }
    ctx.matcher.observe_char(commit, now);
}

/// Play a multi-step sequence: type each step, submit it, wait, type the next.
///
/// Each step gets its own freshly-sampled schedule, so the cadence model isn't
/// reused across a two-minute gap. The wait is polled in short slices rather
/// than slept through in one go, because the kill-switch and the pause control
/// have to stay responsive while the sequence is parked — the whole point is
/// that the user is watching an agent work and may want to intervene.
fn play_sequence(
    sequence: &[Step],
    profile: &crate::typer::Profile,
    base_opts: &ScheduleOptions,
    injector: &mut dyn Injector,
    control: &PlaybackControl,
    focus_lost: Option<&dyn Fn() -> bool>,
) -> crate::typer::PlayOutcome {
    use crate::typer::PlayOutcome;
    /// Poll slice while parked between steps.
    const WAIT_SLICE: std::time::Duration = std::time::Duration::from_millis(50);

    let mut visible_chars = 0usize;
    for (i, step) in sequence.iter().enumerate() {
        let mut step_profile = *profile;
        // A step with a follow-up must be sent, or the wait is meaningless.
        if step.submit() {
            step_profile.send_final_enter = true;
        }
        let mut opts = *base_opts;
        // The "I'm thinking" beat belongs before the first keystroke only; a
        // follow-up already had a wait of its own.
        opts.include_pre_typing_pause = base_opts.include_pre_typing_pause && i == 0;

        let mut rng = ChaCha8Rng::from_entropy();
        let scheduled = schedule(&step.body, &step_profile, &opts, &mut rng);
        let outcome = play_controlled(&scheduled, injector, control, focus_lost);
        visible_chars += outcome.visible_chars;
        if !outcome.completed {
            return PlayOutcome {
                completed: false,
                visible_chars,
                focus_changed: outcome.focus_changed,
            };
        }

        let Some(wait) = step.wait_after else {
            continue;
        };
        tracing::info!(
            target: "prompt_player::fire",
            step = i + 1,
            of = sequence.len(),
            wait_ms = wait.as_millis() as u64,
            "step sent; waiting before the follow-up"
        );
        let mut deadline = Instant::now() + wait;
        while Instant::now() < deadline {
            if control.is_cancelled() {
                injector.release_all_modifiers();
                return PlayOutcome {
                    completed: false,
                    visible_chars,
                    focus_changed: false,
                };
            }
            if let Some(check) = focus_lost {
                if check() {
                    // The user moved to another app mid-wait. Typing the
                    // follow-up there would be exactly the accident the
                    // focus guard exists to prevent.
                    injector.release_all_modifiers();
                    return PlayOutcome {
                        completed: false,
                        visible_chars,
                        focus_changed: true,
                    };
                }
            }
            // Pausing means "hold everything", so a pause pushes the deadline
            // out instead of quietly consuming the wait the author asked for.
            if control.is_paused() {
                std::thread::sleep(WAIT_SLICE);
                deadline += WAIT_SLICE;
                continue;
            }
            std::thread::sleep(WAIT_SLICE.min(deadline.saturating_duration_since(Instant::now())));
        }
    }
    PlayOutcome {
        completed: true,
        visible_chars,
        focus_changed: false,
    }
}

struct PlaybackEndGuard {
    state: Arc<crate::state::AppState>,
}

impl PlaybackEndGuard {
    fn new(state: Arc<crate::state::AppState>) -> Self {
        Self { state }
    }
}

impl Drop for PlaybackEndGuard {
    fn drop(&mut self) {
        self.state.end_playback();
    }
}

fn classify_target_app(ctx: &scopes::ForegroundContext, rdp: RdpMode) -> TargetAppKind {
    if rdp == RdpMode::HostSide {
        return TargetAppKind::Rdp;
    }
    // Lowercased: bundle ids are capitalised (`com.apple.Safari`), so the old
    // case-sensitive `contains("safari")` never matched anything.
    let bundle = ctx.bundle_id.as_deref().unwrap_or("").to_lowercase();
    let bundle = bundle.as_str();
    let exe = ctx
        .executable
        .as_deref()
        .map(|s| s.rsplit(['/', '\\']).next().unwrap_or(s).to_lowercase())
        .unwrap_or_default();
    if is_browser(bundle, &exe) {
        return TargetAppKind::Browser;
    }
    if !bundle.is_empty() || !exe.is_empty() {
        return TargetAppKind::Native;
    }
    TargetAppKind::Unknown
}

/// Lowercased bundle-id substrings, so channel variants hit the same arm.
/// Each is distinctive enough not to collide — bare `"arc"` matches `monarch`.
const BROWSER_BUNDLES: &[&str] = &[
    "safari",
    "chrome",
    "chromium",
    "firefox",
    "edgemac",
    "brave",
    "thebrowser",
    "operasoftware",
    "vivaldi",
    "zen-browser",
    "duckduckgo",
];

/// Windows executable names for browsers, lowercased with extension.
const BROWSER_EXES: &[&str] = &[
    "chrome.exe",
    "chromium.exe",
    "firefox.exe",
    "msedge.exe",
    "brave.exe",
    "opera.exe",
    "opera_gx.exe",
    "vivaldi.exe",
    "arc.exe",
    "iexplore.exe",
    "zen.exe",
];

/// The original list covered five browsers and three Windows exes, and matched
/// nothing in 67 days of field data on a tool whose whole job is web demos.
fn is_browser(bundle: &str, exe: &str) -> bool {
    BROWSER_BUNDLES.iter().any(|b| bundle.contains(b)) || BROWSER_EXES.contains(&exe)
}

#[cfg(test)]
mod clipboard_gate_tests {
    use super::references_clipboard;

    #[test]
    fn reads_only_when_the_body_actually_references_it() {
        for body in [
            "paste this: $CLIPBOARD",
            "wrapped ${CLIPBOARD} form",
            "expression ${{ clipboard.trim() }}",
            "${{ clipboard }}",
        ] {
            assert!(references_clipboard(body), "{body:?} must read");
        }
    }

    #[test]
    fn prose_mentioning_the_clipboard_does_not_read_it() {
        // Reading the clipboard is a privacy-relevant act; a prompt that only
        // talks about clipboards must not trigger one on every fire.
        for body in [
            "Copy the result to your clipboard when done.",
            "CLIPBOARD handling is out of scope for this review.",
            "Explain how the clipboard works in Wayland.",
            "${{ now.toISOString() }} — then paste from the clipboard",
        ] {
            assert!(!references_clipboard(body), "{body:?} must not read");
        }
    }

    #[test]
    fn handles_an_unterminated_expression_block() {
        assert!(references_clipboard("${{ clipboard"));
        assert!(!references_clipboard("${{ now"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(bundle: &str, exe: &str) -> scopes::ForegroundContext {
        scopes::ForegroundContext {
            bundle_id: (!bundle.is_empty()).then(|| bundle.to_string()),
            executable: (!exe.is_empty()).then(|| exe.to_string()),
            ..Default::default()
        }
    }

    fn kind(bundle: &str, exe: &str) -> TargetAppKind {
        classify_target_app(&ctx(bundle, exe), RdpMode::Off)
    }

    #[test]
    fn classifies_macos_browsers_by_bundle_id() {
        for b in [
            "com.apple.Safari",
            "com.google.Chrome",
            "com.google.Chrome.canary",
            "org.mozilla.firefox",
            "com.microsoft.edgemac",
            "com.brave.Browser",
            "company.thebrowser.Browser",
            "com.operasoftware.Opera",
            "com.vivaldi.Vivaldi",
            "app.zen-browser.zen",
            "com.duckduckgo.macos.browser",
        ] {
            assert_eq!(kind(b, ""), TargetAppKind::Browser, "bundle {b}");
        }
    }

    #[test]
    fn classifies_windows_browsers_by_exe() {
        for e in [
            "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
            "C:\\Program Files\\Mozilla Firefox\\firefox.exe",
            "C:\\Program Files (x86)\\Microsoft\\Edge\\msedge.exe",
            "C:\\Users\\x\\AppData\\Local\\BraveSoftware\\brave.exe",
            "C:\\Program Files\\Vivaldi\\Application\\vivaldi.exe",
            "C:\\Users\\x\\AppData\\Local\\Programs\\Opera\\opera.exe",
        ] {
            assert_eq!(kind("", e), TargetAppKind::Browser, "exe {e}");
        }
    }

    #[test]
    fn non_browsers_are_native() {
        // Electron apps embed Chromium but are native windows to the user.
        for (b, e) in [
            ("com.tinyspeck.slackmacgap", ""),
            ("com.microsoft.VSCode", ""),
            ("com.apple.Terminal", ""),
            // Substring near-misses the bundle list must not claim.
            ("com.monarch.app", ""),
            ("com.example.frozen", ""),
            ("", "C:\\Program Files\\Slack\\slack.exe"),
            ("", "C:\\Windows\\System32\\notepad.exe"),
        ] {
            assert_eq!(kind(b, e), TargetAppKind::Native, "{b} {e}");
        }
    }

    #[test]
    fn unknown_when_nothing_identifies_the_app() {
        assert_eq!(kind("", ""), TargetAppKind::Unknown);
    }

    #[test]
    fn rdp_wins_over_everything() {
        assert_eq!(
            classify_target_app(&ctx("com.google.Chrome", ""), RdpMode::HostSide),
            TargetAppKind::Rdp
        );
    }

    #[test]
    fn exe_match_is_exact_not_substring() {
        // `notchrome.exe` must not pass just because it ends in chrome.exe.
        assert_eq!(kind("", "C:\\tools\\notchrome.exe"), TargetAppKind::Native);
    }
}

#[cfg(test)]
mod sequence_tests {
    use super::*;
    use crate::typer::{Profile, RecordingInjector};
    use std::sync::atomic::Ordering;

    /// A profile fast enough to keep the sequence tests near-instant, with the
    /// realism dials that would otherwise add seconds turned off.
    fn quick_profile() -> Profile {
        Profile {
            iki_scale: 0.0,
            iki_min_ms: 0.0,
            pause_scale: 0.0,
            pause_variance_scale: 0.0,
            typos_enabled: false,
            burst_enabled: false,
            rephrase_enabled: false,
            pre_submit_pause_enabled: false,
            ..Profile::FAST_PRESENTER
        }
    }

    fn no_pause_opts() -> ScheduleOptions {
        ScheduleOptions {
            rdp_mode: false,
            include_pre_typing_pause: false,
            newline_mode: Default::default(),
        }
    }

    /// Reconstruct the typed text from a recording injector.
    fn typed(inj: &RecordingInjector) -> String {
        inj.events
            .iter()
            .filter_map(|k| match k {
                Key::Char(c) => Some(*c),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_sequence_types_every_step_and_submits_all_but_the_last() {
        let sequence = steps::split_steps("first\n<!-- pp:wait 10ms -->\nsecond");
        assert_eq!(sequence.len(), 2);
        let mut inj = RecordingInjector::default();
        let control = PlaybackControl::new();
        let outcome = play_sequence(
            &sequence,
            &quick_profile(),
            &no_pause_opts(),
            &mut inj,
            &control,
            None,
        );
        assert!(outcome.completed);
        assert_eq!(typed(&inj), "firstsecond");
        // Exactly one submitting Enter: after step one, not after step two.
        let enters = inj
            .events
            .iter()
            .filter(|k| matches!(k, Key::Enter))
            .count();
        assert_eq!(enters, 1, "events: {:?}", inj.events);
    }

    #[test]
    fn cancelling_during_the_wait_stops_before_the_follow_up() {
        // The kill-switch has to reach a sequence that is parked between
        // steps, or a cancelled demo would still type its follow-up.
        let sequence = steps::split_steps("first\n<!-- pp:wait 30s -->\nsecond");
        let mut inj = RecordingInjector::default();
        let control = PlaybackControl::new();
        let canceller = control.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(120));
            canceller.cancel();
        });
        let started = Instant::now();
        let outcome = play_sequence(
            &sequence,
            &quick_profile(),
            &no_pause_opts(),
            &mut inj,
            &control,
            None,
        );
        assert!(!outcome.completed);
        assert_eq!(typed(&inj), "first", "the follow-up must not be typed");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "cancel should not wait out the full 30s: {:?}",
            started.elapsed()
        );
        assert!(inj.modifier_releases > 0, "modifiers released on abort");
    }

    #[test]
    fn losing_focus_during_the_wait_abandons_the_follow_up() {
        let sequence = steps::split_steps("first\n<!-- pp:wait 30s -->\nsecond");
        let mut inj = RecordingInjector::default();
        let control = PlaybackControl::new();
        // Focus is reported lost only once the first step is on screen, so the
        // abort happens during the wait rather than before any typing.
        let moved = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = moved.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(120));
            flag.store(true, Ordering::Relaxed);
        });
        let lost = || moved.load(Ordering::Relaxed);
        let outcome = play_sequence(
            &sequence,
            &quick_profile(),
            &no_pause_opts(),
            &mut inj,
            &control,
            Some(&lost),
        );
        assert!(!outcome.completed);
        assert!(outcome.focus_changed, "the reason must be reported");
        assert_eq!(typed(&inj), "first");
    }

    #[test]
    fn a_single_step_sequence_is_not_force_submitted() {
        let sequence = steps::split_steps("just one");
        let mut inj = RecordingInjector::default();
        let outcome = play_sequence(
            &sequence,
            &quick_profile(),
            &no_pause_opts(),
            &mut inj,
            &PlaybackControl::new(),
            None,
        );
        assert!(outcome.completed);
        assert_eq!(typed(&inj), "just one");
        assert!(
            !inj.events.iter().any(|k| matches!(k, Key::Enter)),
            "a lone step follows the picker mode, so no implicit Enter"
        );
    }
}
