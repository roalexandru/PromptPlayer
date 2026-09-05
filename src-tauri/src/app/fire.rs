//! FireService — the one pipeline from prompt id to typed output, shared by
//! the hook, the picker, the tray and per-prompt hotkeys.
//!
//! Numbered stage comments below follow: capture foreground → pick candidate →
//! expressions → placeholders → filters → case → RDP → schedule → play → undo.

use crate::app::context::AppContext;
use crate::filters;
use crate::inject::{paste_via_clipboard, EnigoInjector};
use crate::matcher;
use crate::prompts::expressions::ExprContext;
use crate::prompts::placeholders::{expand, PlaceholderContext};
use crate::prompts::Prompt;
use crate::rdp::RdpMode;
use crate::scopes;
use crate::telemetry::{
    self, CancelReason, CharBucket, DurationBucket, InjectionStage, PromptMode, TargetAppKind,
    TelemetryEvent,
};
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

    /// Fire from the picker. `mode` is the modifier-on-Enter behavior; the
    /// pipeline (scope, expressions, filters) is otherwise identical.
    pub fn fire_from_picker(&self, prompt_id: &str, mode: PickMode) {
        self.fire_selected(prompt_id, mode, PromptMode::Picker);
    }

    /// Fire a pinned prompt clicked in the tray. Same pipeline as the picker;
    /// only the reported mode differs, so tray use is finally distinguishable.
    pub fn fire_from_tray(&self, prompt_id: &str, mode: PickMode) {
        self.fire_selected(prompt_id, mode, PromptMode::Tray);
    }

    fn fire_selected(&self, prompt_id: &str, mode: PickMode, telem_mode: PromptMode) {
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
                svc.run_resolved(prompt, None, telem_mode, foreground, Some(mode));
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
                svc.run_resolved(prompt, None, PromptMode::Hotkey, foreground, None);
            })
            .expect("spawn hotkey fire thread");
    }

    /// Run a resolved fire on the dispatch thread. Bails if a playback is
    /// already typing, so two fires can't interleave into one window.
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
                let Some(cancel) = app_state.begin_playback() else {
                    // A playback is already running — don't stomp it.
                    return;
                };
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
    // 1+2 done by the caller. Read the clipboard only when referenced — no
    // reason to touch the user's clipboard on every fire.
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
    let expansion =
        crate::prompts::expressions::expand_expressions_reporting(&prompt.body, &expr_ctx);
    let has_expressions = expansion.had_expressions;
    for err in &expansion.errors {
        telemetry::send(&app, TelemetryEvent::ExpressionError { kind: err.kind() });
    }
    let body_after_expr = expansion.text;

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
        // Only meaningful after a suppressed `>`; picker selections type into
        // an already-restored app.
        include_pre_typing_pause: typed_form.is_some(),
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
            _ => {}
        }
    }
    // §9.3 — RDP clipboard sync is unreliable and can land the body on the
    // wrong side, so paste falls back to typing.
    if paste_mode && rdp_mode == RdpMode::HostSide {
        tracing::info!("paste demoted to typed flow under RDP host-side mode");
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
        let outcome = play_guarded(&scheduled, &mut inj, cancel.clone(), Some(&focus_lost));
        completed_chars = outcome.visible_chars;
        focus_changed = outcome.focus_changed;
        drop(inj);
        outcome.completed
    };

    if completed {
        let mut undo_offered = false;
        if let Some(form) = typed_form.as_deref() {
            if profile.send_final_enter {
                // Backspace-undo can't un-send a submitted message, and the
                // backspaces would hit whatever field has focus now.
                tracing::debug!("not recording undo: prompt was submitted via final Enter");
            } else {
                undo.record(form.to_string(), body_chars);
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
