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
use crate::typer::{play, schedule, Injector, Key, ScheduleOptions};
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
    pub fn fire_from_trigger(&self, candidate_ids: Vec<String>, typed_form: String) {
        let foreground = scopes::capture_foreground_context();
        let candidates: Vec<Prompt> = {
            let all = self.ctx.prompts.read();
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
            tracing::debug!("no scope match for trigger; nothing fires");
            telemetry::send(
                &self.app,
                TelemetryEvent::PromptCancelled {
                    reason: CancelReason::Error,
                    completed_chars_pct: 0,
                },
            );
            return;
        };
        let Some(prompt) = candidates.into_iter().find(|p| p.id == picked_id) else {
            tracing::warn!("picked prompt {} not found", picked_id);
            return;
        };
        self.spawn_fire(
            prompt,
            Some(typed_form),
            PromptMode::Stealth,
            foreground,
            None,
        );
    }

    /// Fire a prompt selected from the picker. Mode controls modifier-on-Enter
    /// behavior (human cadence, fast, paste, run). Picker runs scope/expr/
    /// filters too — it's the same pipeline.
    pub fn fire_from_picker(&self, prompt_id: &str, mode: PickMode) {
        let Some(prompt) = self.ctx.prompts.find(prompt_id) else {
            tracing::warn!("picker selected unknown prompt {}", prompt_id);
            return;
        };
        if !prompt.enabled {
            tracing::info!("picker selected disabled prompt {}; ignoring", prompt_id);
            return;
        }
        // Picker runs after focus restore — capture foreground anew so
        // scope/expr context reflects the target app, not Prompt Player itself.
        let foreground = scopes::capture_foreground_context();
        self.spawn_fire(prompt, None, PromptMode::Picker, foreground, Some(mode));
    }

    /// Fire from a per-prompt global hotkey. No commit-char, no typed form.
    pub fn fire_from_hotkey(&self, prompt_id: &str) {
        let Some(prompt) = self.ctx.prompts.find(prompt_id) else {
            return;
        };
        if !prompt.enabled {
            return;
        }
        let foreground = scopes::capture_foreground_context();
        self.spawn_fire(prompt, None, PromptMode::Stealth, foreground, None);
    }

    fn spawn_fire(
        &self,
        prompt: Prompt,
        typed_form: Option<String>,
        telem_mode: PromptMode,
        foreground: scopes::ForegroundContext,
        pick_mode: Option<PickMode>,
    ) {
        let cancel = self.ctx.state.begin_playback();
        let app_state = self.ctx.state.clone();
        let undo = self.ctx.undo.clone();
        let rdp_registry = self.ctx.rdp.clone();
        let app = self.app.clone();
        let thread_name = match telem_mode {
            PromptMode::Stealth => "prompt-player-fire-stealth",
            PromptMode::Picker => "prompt-player-fire-picker",
        };
        thread::Builder::new()
            .name(thread_name.into())
            .spawn(move || {
                run_fire_pipeline(
                    app,
                    prompt,
                    typed_form,
                    telem_mode,
                    foreground,
                    pick_mode,
                    cancel,
                    app_state,
                    undo,
                    rdp_registry,
                );
            })
            .expect("spawn fire thread");
    }

    /// Backspace-undo flow — separate from fire because it doesn't run the
    /// pipeline; it just types backspaces + retypes the original trigger.
    pub fn run_undo(&self) {
        let app_state = self.ctx.state.clone();
        let undo = self.ctx.undo.clone();
        let app = self.app.clone();
        thread::Builder::new()
            .name("prompt-player-undo".into())
            .spawn(move || {
                let Some(entry) = undo.take_recent(std::time::Instant::now()) else {
                    return;
                };
                let cancel = app_state.begin_playback();
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
                for c in entry.trigger_form.chars() {
                    if cancel.load(Ordering::Relaxed) {
                        inj.release_all_modifiers();
                        app_state.end_playback();
                        return;
                    }
                    inj.type_char(c);
                    thread::sleep(std::time::Duration::from_millis(20));
                }
                app_state.end_playback();
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
    // 1+2 — already done by caller (foreground capture, scope pick).
    // 3 — TS expressions.
    let mut expr_ctx = ExprContext::default();
    expr_ctx.app_bundle = foreground.bundle_id.clone();
    expr_ctx.app_name = foreground
        .executable
        .as_deref()
        .and_then(|s| std::path::Path::new(s).file_name().and_then(|f| f.to_str()))
        .map(|s| s.to_string());
    expr_ctx.window_title = foreground.window_title.clone();
    let body_after_expr = crate::prompts::expressions::expand_expressions(&prompt.body, &expr_ctx);
    let has_expressions = body_after_expr.len() != prompt.body.len();

    // 4 — placeholders.
    let mut ph_ctx = PlaceholderContext::default();
    ph_ctx.app_bundle = foreground.bundle_id.clone();
    ph_ctx.app_name = expr_ctx.app_name.clone();
    ph_ctx.window_title = foreground.window_title.clone();
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
                profile.iki_scale = 0.20;
                profile.typos_enabled = false;
                profile.pre_submit_pause_enabled = false;
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
                Ok(()) => true,
                Err(e) => {
                    tracing::error!("clipboard paste failed: {:?}", e);
                    false
                }
            }
        }
    } else {
        let mut inj = match EnigoInjector::new() {
            Ok(i) => i,
            Err(e) => {
                tracing::error!("enigo init failed: {:?}", e);
                app_state.end_playback();
                return;
            }
        };
        let result = play(&scheduled, &mut inj, cancel.clone());
        drop(inj);
        result
    };

    if completed {
        if let Some(form) = typed_form.as_deref() {
            undo.record(form.to_string(), body_chars);
        }
    } else {
        // Paste is effectively atomic — `completed=false` means we never
        // got past the cancel check or the clipboard/Ctrl+V failed before
        // any character landed. Report 0% so telemetry doesn't claim a
        // full paste happened when nothing did. The typed-path estimator
        // uses `remaining_chars(&scheduled)` (still coarse — see the
        // function comment).
        let pct = if paste_mode || body_chars == 0 {
            0
        } else {
            (body_chars.saturating_sub(remaining_chars(&scheduled)) * 100 / body_chars).min(100)
                as u8
        };
        telemetry::send(
            &app,
            TelemetryEvent::PromptCancelled {
                reason: CancelReason::UserKeystrokes,
                completed_chars_pct: pct,
            },
        );
    }
    app_state.end_playback();
}

fn classify_target_app(ctx: &scopes::ForegroundContext, rdp: RdpMode) -> TargetAppKind {
    if rdp == RdpMode::HostSide {
        return TargetAppKind::Rdp;
    }
    let bundle = ctx.bundle_id.as_deref().unwrap_or("");
    let exe = ctx
        .executable
        .as_deref()
        .map(|s| {
            s.rsplit(|c| c == '/' || c == '\\')
                .next()
                .unwrap_or(s)
                .to_lowercase()
        })
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

fn remaining_chars(_schedule: &[crate::typer::ScheduledKey]) -> usize {
    // We don't track current playback index; approximate as 0 (everything
    // remaining was cancelled). This is a coarse signal — the bucket is
    // what's reported, not exact counts.
    0
}
