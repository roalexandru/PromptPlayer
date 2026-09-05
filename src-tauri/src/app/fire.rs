//! FireService — the single pipeline that takes a prompt id (and the
//! user-typed form, if any) and produces typed output.
//!
//! Used by:
//! - The keyboard hook's `on_fire` callback (trigger word + commit char).
//! - The picker's `ipc_picker_select` (mode = human/fast/paste/run).
//! - Per-prompt global hotkeys (no commit char, no typed form).
//!
//! Pipeline:
//! 1. Capture `ForegroundContext` (what app is focused).
//! 2. Pick best candidate via `scopes::pick_best` (priority + specificity).
//! 3. Pre-flight the focused element (§11): refuse to type into a password
//!    field or a surface that can't take text at all.
//! 4. Gather the context the body actually references — clipboard, selection,
//!    repo/branch — each read lazily so an unrelated prompt pays nothing.
//! 5. Expand `${{ TS expr }}` blocks, then VS Code-style placeholders.
//! 6. Apply filter chain.
//! 7. Apply case propagation (only when there's a typed form).
//! 8. Detect RDP host-side mode → schedule with floor + multiplier.
//! 9. Resolve the newline gesture for the target (chat vs terminal agent).
//! 10. Pre-compute schedule from profile + overrides.
//! 11. Play the schedule under a `PlaybackControl` (cancel / pause / speed).
//! 12. On completion, record undo entry and usage history.

use crate::accessibility;
use crate::app::context::AppContext;
use crate::filters;
use crate::inject::{paste_via_clipboard, EnigoInjector};
use crate::matcher;
use crate::prompts::expressions::ExprContext;
use crate::prompts::placeholders::{expand, PlaceholderContext};
use crate::prompts::Prompt;
use crate::rdp::RdpMode;
use crate::repo;
use crate::scopes;
use crate::telemetry::{self, CancelReason, CharBucket, PromptMode, TargetAppKind, TelemetryEvent};
use crate::typer::{play_controlled, schedule, Injector, Key, PlaybackControl, ScheduleOptions};
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use tauri::AppHandle;

/// Pickup mode used when firing from the picker (Spec §5.3 modifier-on-Enter).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickMode {
    Human,
    Fast,
    Paste,
    /// §5.3 Cmd+Enter — type the body, then submit it. The one mode the spec
    /// listed and the picker never had; it is also what makes the app useful
    /// against a coding agent, where "typed but not sent" is only half a turn.
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

/// Everything a single fire needs that isn't shared app state. Bundled so the
/// pipeline takes four arguments instead of a dozen positional ones.
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

    /// Fire a prompt triggered by the keyboard hook (commit-char + typed form).
    /// `candidate_ids` may contain multiple prompts sharing this trigger;
    /// `pick_best` resolves by scope+priority+specificity.
    ///
    /// Called directly from the OS keyboard-hook callback, which on Windows
    /// runs under the `LowLevelHooksTimeout` budget (~300ms): exceeding it
    /// makes Windows silently unhook us. So we do NO work here beyond spawning
    /// a dispatch thread — foreground capture, the prompt-store read, and
    /// scope resolution all happen off the hook thread.
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
                    // The trigger matched (so the hook already suppressed the
                    // commit char and popped the trigger from the ring), but no
                    // candidate's scope matches the current app. Put the eaten
                    // commit char back so the user isn't left with a silently
                    // missing `>`, and re-observe trigger+commit so the ring
                    // mirrors the screen again. All candidates share the same
                    // commit char (the matcher filters on it).
                    let commit = candidates[0].commit_char;
                    tracing::debug!("no scope match for trigger; re-injecting '{}'", commit);
                    restore_suppressed_commit(&svc.ctx, &typed_form, commit);
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

    /// Fire a prompt selected from the picker. Mode controls modifier-on-Enter
    /// behavior (human cadence, fast, paste, run). Picker runs scope/expr/
    /// filters too — it's the same pipeline.
    pub fn fire_from_picker(&self, prompt_id: &str, mode: PickMode) {
        self.fire_from_picker_with(prompt_id, mode, HashMap::new());
    }

    /// Picker fire carrying pre-resolved tab-stop / choice answers.
    pub fn fire_from_picker_with(
        &self,
        prompt_id: &str,
        mode: PickMode,
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
                    telem_mode: PromptMode::Picker,
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
                svc.run_resolved(FireRequest {
                    prompt,
                    typed_form: None,
                    telem_mode: PromptMode::Stealth,
                    foreground,
                    pick_mode: None,
                    stop_answers: HashMap::new(),
                });
            })
            .expect("spawn hotkey fire thread");
    }

    /// Run a fully-resolved fire on the current (dispatch) thread. Acquires the
    /// playback lock; bails if another playback is already typing so keystrokes
    /// can't interleave into the same window.
    fn run_resolved(&self, req: FireRequest) {
        let Some(control) = self.ctx.state.begin_playback() else {
            tracing::info!("fire ignored — a playback is already in progress");
            return;
        };
        run_fire_pipeline(self.app.clone(), self.ctx.clone(), req, control);
    }

    /// Backspace-undo flow — separate from fire because it doesn't run the
    /// pipeline. The trigger word itself was never erased during expansion
    /// (the body is typed as a continuation after it), so undo only needs to
    /// backspace the body chars; the user's original trigger stays on screen.
    /// We then re-observe the trigger into the matcher buffer (which had it
    /// popped at fire time) so `trigger>` can fire again immediately after.
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
                let mut inj = match EnigoInjector::new() {
                    Ok(i) => i,
                    Err(e) => {
                        tracing::error!("undo enigo init failed: {:?}", e);
                        app_state.end_playback();
                        return;
                    }
                };
                for _ in 0..entry.body_chars_typed {
                    if control.is_cancelled() {
                        inj.release_all_modifiers();
                        app_state.end_playback();
                        return;
                    }
                    inj.press_backspace();
                    thread::sleep(std::time::Duration::from_millis(15));
                }
                app_state.end_playback();
                // Restore the matcher's shadow of the screen: the trigger text
                // is back on screen (we never erased it), but it was popped
                // from the ring at fire time. Re-observe it so a fresh `>`
                // can re-fire without the user retyping the trigger.
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
    let clipboard = if body_src.contains("CLIPBOARD") || body_src.contains("clipboard") {
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

    // 5 — TS expressions, then placeholders.
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
        ..Default::default()
    };
    let body_after_expr = crate::prompts::expressions::expand_expressions(&prompt.body, &expr_ctx);
    let has_expressions = body_after_expr.len() != prompt.body.len();

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
        // Pre-typing pause is only meaningful for the trigger path
        // (after the suppressed `>`). Picker selections type from a
        // restored-focus app — no pause needed.
        include_pre_typing_pause: typed_form.is_some(),
        newline_mode,
    };
    let mut paste_mode = false;
    if let Some(mode) = pick_mode {
        opts.include_pre_typing_pause = false;
        match mode {
            PickMode::Fast => {
                // Picker "Fast" is more aggressive than the FAST_PRESENTER
                // named profile: this is "I want the body NOW but still want
                // it to look typed, not pasted". Numbers are tuned so every
                // dial actually takes effect (low `iki_min_ms` keeps the
                // global floor from clamping `iki_scale` away) and so word
                // / sentence pauses don't visibly stall — they shrink with
                // the IKI via `pause_scale`. Burst is disabled because at
                // this floor it adds no perceptual variety.
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
            PickMode::Human => {}
        }
    }
    // RDP host-side: clipboard sync to the remote session is unreliable
    // (spec §9.3), so a real clipboard paste can silently land the body on
    // the wrong side. Demote paste to the human-typed path in that case.
    if paste_mode && rdp_mode == RdpMode::HostSide {
        tracing::info!("paste demoted to typed flow under RDP host-side mode");
        paste_mode = false;
    }

    let target_app = classify_target_app(&foreground, rdp_mode);
    let scope_match = !matches!(target_app, TargetAppKind::Unknown);

    // 8 — schedule. Skipped entirely in paste mode: a clipboard paste
    // dispatches in one Ctrl/Cmd+V keystroke, no per-key cadence to build,
    // and the previous code threw the schedule away anyway after computing
    // it. That alone shaves tens of ms off the paste cold-start on big
    // prompts. The RNG is only seeded when the schedule is actually built,
    // saving an entropy-pool draw on every paste fire.
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

    telemetry::send(
        &app,
        TelemetryEvent::PromptFired {
            mode: telem_mode,
            char_count_bucket: CharBucket::classify(body_chars),
            has_expressions,
            target_app_kind: target_app,
            scope_match,
        },
    );

    // 9 — play.
    // Snapshot the target app's identity so we can bail if focus leaves it
    // mid-playback (a click, Alt/Cmd-Tab, a notification) — otherwise the
    // remainder of the prompt gets typed into whatever now has focus (a chat
    // with a customer, a terminal, a password field). The §2.6 panic ring only
    // catches *typed* keystrokes; a silent focus change produces none.
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
        // Clipboard paste: save → set → Ctrl/Cmd+V → wait → restore.
        // Focus was confirmed by the caller (`picker_select` →
        // `FocusStore::restore_and_wait`), so the synthesized paste
        // keystroke lands on the right window. Cancel is checked once
        // before we touch the clipboard — once Ctrl/Cmd+V is in flight
        // the paste is effectively atomic, so per-char cancellation
        // doesn't apply.
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
                return;
            }
        };
        // Hold 1ms timer resolution across the whole typed run so per-key
        // sleeps don't quantize to the OS timer period (Windows-only effect).
        let _timer = crate::typer::TimerResolutionGuard::acquire();
        let outcome = play_controlled(&scheduled, &mut inj, &control, Some(&focus_lost));
        completed_chars = outcome.visible_chars;
        focus_changed = outcome.focus_changed;
        drop(inj);
        outcome.completed
    };

    if completed {
        // §5.2 recents tier — a *completed* fire is a use; a cancelled one is
        // not, so this sits inside the success branch.
        ctx.usage.record(&prompt.id);
        if let Some(form) = typed_form.as_deref() {
            if profile.send_final_enter {
                // The profile pressed Enter to submit the message. Backspace-
                // undo can't un-send it, and the backspaces would delete from
                // whatever field now has focus (often empty) — so don't arm
                // undo for a submitted prompt.
                tracing::debug!("not recording undo: prompt was submitted via final Enter");
            } else {
                ctx.undo.record(form.to_string(), body_chars);
            }
        }
    } else {
        // Paste is effectively atomic — `completed=false` means we never
        // got past the cancel check or the clipboard/Ctrl+V failed before
        // any character landed. Report 0% so telemetry doesn't claim a
        // full paste happened when nothing did. The typed-path estimator
        // uses `remaining_chars(&scheduled)` (still coarse — see the
        // function comment).
        let pct = if body_chars == 0 {
            0
        } else {
            (completed_chars * 100 / body_chars).min(100) as u8
        };
        let reason = if focus_changed {
            CancelReason::FocusChanged
        } else {
            CancelReason::UserKeystrokes
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
    let bundle = ctx.bundle_id.as_deref().unwrap_or("");
    let exe = ctx
        .executable
        .as_deref()
        .map(|s| s.rsplit(['/', '\\']).next().unwrap_or(s).to_lowercase())
        .unwrap_or_default();
    let is_browser = bundle.contains("safari")
        || bundle.contains("chrome")
        || bundle.contains("firefox")
        || bundle.contains("edge")
        || bundle.contains("brave")
        || bundle.contains("arc")
        || exe == "chrome.exe"
        || exe == "firefox.exe"
        || exe == "msedge.exe";
    if is_browser {
        return TargetAppKind::Browser;
    }
    if !bundle.is_empty() || !exe.is_empty() {
        return TargetAppKind::Native;
    }
    TargetAppKind::Unknown
}
