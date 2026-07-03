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
//! 3. Expand `${{ TS expr }}` blocks (with the foreground context).
//! 4. Expand VS Code-style placeholders.
//! 5. Apply filter chain.
//! 6. Apply case propagation (only when there's a typed form).
//! 7. Detect RDP host-side mode → schedule with floor + multiplier.
//! 8. Pre-compute schedule from profile + overrides.
//! 9. Spawn a typer thread; play schedule via Injector.
//! 10. On completion, record undo entry (typed form + body length).

use crate::app::context::AppContext;
use crate::filters;
use crate::inject::{paste_via_clipboard, EnigoInjector};
use crate::matcher;
use crate::prompts::expressions::ExprContext;
use crate::prompts::placeholders::{expand, PlaceholderContext};
use crate::prompts::Prompt;
use crate::rdp::RdpMode;
use crate::scopes;
use crate::telemetry::{self, CancelReason, CharBucket, PromptMode, TargetAppKind, TelemetryEvent};
use crate::typer::{play_guarded, schedule, Injector, Key, ScheduleOptions};
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use tauri::AppHandle;

/// Pickup mode used when firing from the picker (Spec §5.3 modifier-on-Enter).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickMode {
    Human,
    Fast,
    Paste,
}

impl PickMode {
    pub fn parse(s: &str) -> Self {
        match s {
            "fast" => Self::Fast,
            "paste" => Self::Paste,
            _ => Self::Human,
        }
    }

    pub fn is_paste(&self) -> bool {
        matches!(self, Self::Paste)
    }
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
                svc.run_resolved(
                    prompt,
                    Some(typed_form),
                    PromptMode::Stealth,
                    foreground,
                    None,
                );
            })
            .expect("spawn fire dispatch thread");
    }

    /// Fire a prompt selected from the picker. Mode controls modifier-on-Enter
    /// behavior (human cadence, fast, paste, run). Picker runs scope/expr/
    /// filters too — it's the same pipeline.
    pub fn fire_from_picker(&self, prompt_id: &str, mode: PickMode) {
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
                svc.run_resolved(prompt, None, PromptMode::Picker, foreground, Some(mode));
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
                svc.run_resolved(prompt, None, PromptMode::Stealth, foreground, None);
            })
            .expect("spawn hotkey fire thread");
    }

    /// Run a fully-resolved fire on the current (dispatch) thread. Acquires the
    /// playback lock; bails if another playback is already typing so keystrokes
    /// can't interleave into the same window.
    fn run_resolved(
        &self,
        prompt: Prompt,
        typed_form: Option<String>,
        telem_mode: PromptMode,
        foreground: scopes::ForegroundContext,
        pick_mode: Option<PickMode>,
    ) {
        let Some(cancel) = self.ctx.state.begin_playback() else {
            tracing::info!("fire ignored — a playback is already in progress");
            return;
        };
        run_fire_pipeline(
            self.app.clone(),
            prompt,
            typed_form,
            telem_mode,
            foreground,
            pick_mode,
            cancel,
            self.ctx.state.clone(),
            self.ctx.undo.clone(),
            self.ctx.rdp.clone(),
        );
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
                let Some(cancel) = app_state.begin_playback() else {
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
                    if cancel.load(Ordering::Relaxed) {
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

#[allow(clippy::too_many_arguments)]
fn run_fire_pipeline(
    app: AppHandle,
    prompt: Prompt,
    typed_form: Option<String>,
    telem_mode: PromptMode,
    foreground: scopes::ForegroundContext,
    pick_mode: Option<PickMode>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    app_state: Arc<crate::state::AppState>,
    undo: Arc<crate::undo::UndoLog>,
    rdp_registry: Arc<crate::rdp::RdpRegistry>,
) {
    let _playback_guard = PlaybackEndGuard::new(app_state.clone());
    // 1+2 — already done by caller (foreground capture, scope pick).
    // Read the clipboard only when the body actually references it — avoids
    // touching the user's clipboard on every fire (privacy + cost). Covers both
    // the `$CLIPBOARD` placeholder and the `clipboard` expression builtin.
    let clipboard = if prompt.body.contains("CLIPBOARD") || prompt.body.contains("clipboard") {
        crate::inject::read_clipboard_text()
    } else {
        None
    };
    // 3 — TS expressions.
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
        ..Default::default()
    };
    let body_after_expr = crate::prompts::expressions::expand_expressions(&prompt.body, &expr_ctx);
    let has_expressions = body_after_expr.len() != prompt.body.len();

    // 4 — placeholders.
    let ph_ctx = PlaceholderContext {
        app_bundle: foreground.bundle_id.clone(),
        app_name: expr_ctx.app_name.clone(),
        window_title: foreground.window_title.clone(),
        clipboard,
        ..Default::default()
    };
    let expanded = expand(&body_after_expr, &ph_ctx);

    // 5 — filter chain.
    let filtered = filters::apply_chain(&expanded.text, &prompt.filters);

    // 6 — case propagation (trigger path only).
    let body = match &typed_form {
        Some(form) => matcher::propagate_case(form, &filtered),
        None => filtered,
    };

    // 7 — RDP detection.
    let rdp_mode = rdp_registry.detect(&foreground);
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
    let mut opts = ScheduleOptions {
        rdp_mode: rdp_mode == RdpMode::HostSide,
        // Pre-typing pause is only meaningful for the trigger path
        // (after the suppressed `>`). Picker selections type from a
        // restored-focus app — no pause needed.
        include_pre_typing_pause: typed_form.is_some(),
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
            _ => {}
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
        if cancel.load(Ordering::Relaxed) {
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
                            let outcome = play_guarded(
                                &fallback_schedule,
                                &mut inj,
                                cancel.clone(),
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
        let outcome = play_guarded(&scheduled, &mut inj, cancel.clone(), Some(&focus_lost));
        completed_chars = outcome.visible_chars;
        focus_changed = outcome.focus_changed;
        drop(inj);
        outcome.completed
    };

    if completed {
        if let Some(form) = typed_form.as_deref() {
            if profile.send_final_enter {
                // The profile pressed Enter to submit the message. Backspace-
                // undo can't un-send it, and the backspaces would delete from
                // whatever field now has focus (often empty) — so don't arm
                // undo for a submitted prompt.
                tracing::debug!("not recording undo: prompt was submitted via final Enter");
            } else {
                undo.record(form.to_string(), body_chars);
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
