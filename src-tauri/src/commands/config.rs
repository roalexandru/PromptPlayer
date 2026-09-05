//! Library-level configuration, setlist, and playback-control IPC (§7.2, §3.5).

use crate::app::context::AppContext;
use crate::app::fire::FireService;
use crate::config::AppConfig;
use crate::error::{into_ipc, AppError, IpcResult};

/// One row of the setlist editor.
#[derive(Debug, Clone, serde::Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct SetlistEntry {
    pub prompt_id: String,
    /// Prompt name, or the raw id when the prompt no longer exists.
    pub name: String,
    /// True when the id no longer resolves — a deleted or renamed prompt still
    /// listed in `promptplayer.yaml`. Surfaced rather than silently dropped so
    /// the user can see why a cue does nothing.
    pub missing: bool,
    /// Index of the cue that fires next.
    pub is_next: bool,
}

#[tauri::command]
#[specta::specta]
pub fn get_config(ctx: tauri::State<'_, AppContext>) -> AppConfig {
    ctx.config.get()
}

/// Persist a full config object and adopt it in memory.
///
/// Everything except the global hotkeys takes effect immediately, because the
/// fire pipeline and picker read the store per use. Hotkeys are registered
/// with the OS once at startup, so a changed chord needs a relaunch; the
/// returned flag lets the UI say so instead of leaving the user guessing.
#[tauri::command]
#[specta::specta]
pub fn save_config(
    config: AppConfig,
    ctx: tauri::State<'_, AppContext>,
) -> IpcResult<SaveConfigOutcome> {
    let previous = ctx.config.get();
    let hotkeys_changed = hotkeys_differ(&previous, &config);
    ctx.config.set(config.clone());
    match crate::config::save(&config) {
        Ok(path) => into_ipc(Ok(SaveConfigOutcome {
            path: path.to_string_lossy().into_owned(),
            restart_required_for_hotkeys: hotkeys_changed,
        })),
        Err(e) => {
            // Roll the in-memory copy back so the UI and disk can't disagree.
            ctx.config.set(previous);
            into_ipc(Err(AppError::Internal(e)))
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct SaveConfigOutcome {
    pub path: String,
    pub restart_required_for_hotkeys: bool,
}

fn hotkeys_differ(a: &AppConfig, b: &AppConfig) -> bool {
    a.hotkey_arm != b.hotkey_arm
        || a.hotkey_picker != b.hotkey_picker
        || a.hotkey_kill != b.hotkey_kill
        || a.hotkey_panic != b.hotkey_panic
        || a.hotkey_next_cue != b.hotkey_next_cue
        || a.hotkey_pause != b.hotkey_pause
        || a.hotkey_faster != b.hotkey_faster
        || a.hotkey_slower != b.hotkey_slower
}

#[tauri::command]
#[specta::specta]
pub fn get_setlist(ctx: tauri::State<'_, AppContext>) -> Vec<SetlistEntry> {
    let cfg = ctx.config.get();
    let next = if cfg.setlist.is_empty() {
        usize::MAX
    } else {
        ctx.state.setlist_cursor() % cfg.setlist.len()
    };
    cfg.setlist
        .iter()
        .enumerate()
        .map(|(i, id)| match ctx.prompts.find(id) {
            Some(p) => SetlistEntry {
                prompt_id: id.clone(),
                name: p.name,
                missing: false,
                is_next: i == next,
            },
            None => SetlistEntry {
                prompt_id: id.clone(),
                name: id.clone(),
                missing: true,
                is_next: i == next,
            },
        })
        .collect()
}

/// Replace the setlist and reset the cue cursor to the top.
#[tauri::command]
#[specta::specta]
pub fn set_setlist(ids: Vec<String>, ctx: tauri::State<'_, AppContext>) -> IpcResult<()> {
    let result = ctx.config.update(|c| c.setlist = ids);
    ctx.state.set_setlist_cursor(0);
    into_ipc(result.map(|_| ()).map_err(AppError::Internal))
}

/// Fire the next cue and advance the cursor. Returns the prompt id that fired.
///
/// This is the gesture the picker can't replace: under stage pressure you stop
/// recalling trigger words, and a fuzzy search is one more thing to get right
/// in front of an audience. One key, next thing.
#[tauri::command]
#[specta::specta]
pub fn fire_next_cue(
    app: tauri::AppHandle,
    ctx: tauri::State<'_, AppContext>,
) -> IpcResult<Option<String>> {
    into_ipc(fire_next_cue_inner(&app, ctx.inner()))
}

/// Shared by the IPC command and the global hotkey handler.
pub fn fire_next_cue_inner(
    app: &tauri::AppHandle,
    ctx: &AppContext,
) -> Result<Option<String>, AppError> {
    let setlist = ctx.config.get().setlist;
    if setlist.is_empty() {
        return Ok(None);
    }
    // Walk forward past any cue whose prompt no longer exists, so one stale
    // entry can't wedge the whole setlist. At most one full lap.
    for _ in 0..setlist.len() {
        let Some(index) = ctx.state.take_next_cue(setlist.len()) else {
            return Ok(None);
        };
        let id = &setlist[index];
        if ctx.prompts.find(id).is_some() {
            FireService::new(ctx.clone(), app.clone()).fire_from_hotkey(id);
            crate::app::shortcuts::refresh_tray_popup(app);
            return Ok(Some(id.clone()));
        }
        tracing::warn!("setlist cue {} no longer exists; skipping", id);
    }
    Err(AppError::InvalidArg(
        "no setlist cue resolves to an existing prompt".into(),
    ))
}

/// Move the cue cursor back to the first entry — "start the demo again".
#[tauri::command]
#[specta::specta]
pub fn reset_setlist(app: tauri::AppHandle, ctx: tauri::State<'_, AppContext>) {
    ctx.state.set_setlist_cursor(0);
    crate::app::shortcuts::refresh_tray_popup(&app);
}

/// Live playback state, for the tray's pause row.
#[derive(Debug, Clone, serde::Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackStatus {
    pub playing: bool,
    pub paused: bool,
    pub speed: f64,
}

#[tauri::command]
#[specta::specta]
pub fn playback_status(ctx: tauri::State<'_, AppContext>) -> PlaybackStatus {
    let control = ctx.state.playback_control();
    PlaybackStatus {
        playing: ctx.state.is_playing(),
        paused: control.is_paused(),
        speed: control.speed(),
    }
}

/// §3.5 — pause or resume the running playback. `None` when nothing is
/// playing, so the caller can leave the UI alone.
#[tauri::command]
#[specta::specta]
pub fn toggle_playback_pause(ctx: tauri::State<'_, AppContext>) -> Option<bool> {
    let paused = ctx.state.toggle_pause();
    if let Some(p) = paused {
        tracing::info!("playback {}", if p { "paused" } else { "resumed" });
    }
    paused
}

/// Speed the running playback up or down one step. Returns the new multiplier.
#[tauri::command]
#[specta::specta]
pub fn nudge_playback_speed(faster: bool, ctx: tauri::State<'_, AppContext>) -> Option<f64> {
    let factor = if faster {
        crate::typer::SPEED_STEP
    } else {
        1.0 / crate::typer::SPEED_STEP
    };
    let speed = ctx.state.nudge_speed(factor);
    if let Some(s) = speed {
        tracing::info!("playback speed → x{:.2}", s);
    }
    speed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    #[test]
    fn hotkey_changes_are_detected() {
        let a = AppConfig::default();
        let mut b = a.clone();
        assert!(!hotkeys_differ(&a, &b));
        b.hotkey_picker = Some("ctrl+alt+p".into());
        assert!(hotkeys_differ(&a, &b));
    }

    #[test]
    fn non_hotkey_changes_do_not_demand_a_restart() {
        // These all read through on the next fire, so the UI must not tell the
        // user to relaunch for them.
        let a = AppConfig::default();
        let mut b = a.clone();
        b.newline_mode = crate::config::NewlineMode::BackslashEnter;
        b.text_field_guard = false;
        b.setlist = vec!["x".into()];
        b.auto_disarm_minutes = 10;
        assert!(!hotkeys_differ(&a, &b));
    }
}
