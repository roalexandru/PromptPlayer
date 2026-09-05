//! "Keep Awake" IPC commands — mirror the `armed` toggle wiring.

use crate::app::context::AppContext;
use crate::app::shortcuts;
use crate::settings::KEEP_AWAKE_CHOICES;
use crate::telemetry::{self, TelemetryEvent};
use tauri::AppHandle;

/// Keep-awake state for the UI: on/off, the chosen auto-off, and how long is
/// left so the tray can render "1h 12m left" instead of a bare checkmark.
#[derive(Debug, Clone, serde::Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct KeepAwakeState {
    pub enabled: bool,
    /// Auto-off for the active session, in minutes. `0` = indefinite.
    pub duration_mins: u16,
    /// Seconds until auto-off, or `None` when off or indefinite.
    pub remaining_secs: Option<u32>,
    /// The user's default auto-off, applied to the next enable.
    pub default_mins: u16,
    /// Selectable durations, so the UI doesn't hardcode its own list.
    pub choices: Vec<u16>,
    /// Whether keep-awake is restored at launch.
    pub restore: bool,
}

fn snapshot(ctx: &AppContext) -> KeepAwakeState {
    let settings = ctx.settings.get();
    KeepAwakeState {
        enabled: ctx.power.is_enabled(),
        duration_mins: ctx.power.duration_mins(),
        remaining_secs: ctx.power.remaining().map(|d| d.as_secs() as u32),
        default_mins: settings.keep_awake_mins,
        choices: KEEP_AWAKE_CHOICES.to_vec(),
        restore: settings.restore_keep_awake,
    }
}

#[tauri::command]
#[specta::specta]
pub fn get_keep_awake(ctx: tauri::State<'_, AppContext>) -> KeepAwakeState {
    snapshot(&ctx)
}

/// Flip keep-awake and apply the OS assertion. Turning on starts a bounded
/// session, which is what stops the multi-day sessions we saw in the field.
#[tauri::command]
#[specta::specta]
pub fn toggle_keep_awake(
    app: AppHandle,
    duration_mins: Option<u16>,
    ctx: tauri::State<'_, AppContext>,
) -> KeepAwakeState {
    let mins = duration_mins.unwrap_or_else(|| ctx.settings.get().keep_awake_mins);
    let enabled = ctx.power.toggle_for(mins);
    ctx.settings.update(|s| {
        s.keep_awake = enabled;
        if enabled {
            s.keep_awake_mins = mins;
        }
    });
    shortcuts::refresh_tray_popup(&app);
    telemetry::send(
        &app,
        TelemetryEvent::KeepAwakeToggled {
            enabled,
            duration_mins: if enabled { mins } else { 0 },
        },
    );
    snapshot(&ctx)
}

/// Change the default auto-off without toggling. Re-arms a running session so
/// picking "30 min" while already on takes effect immediately.
#[tauri::command]
#[specta::specta]
pub fn set_keep_awake_duration(
    app: AppHandle,
    duration_mins: u16,
    ctx: tauri::State<'_, AppContext>,
) -> KeepAwakeState {
    ctx.settings.update(|s| s.keep_awake_mins = duration_mins);
    if ctx.power.is_enabled() {
        ctx.power.set_for(true, duration_mins);
    }
    shortcuts::refresh_tray_popup(&app);
    snapshot(&ctx)
}

/// Opt in/out of restoring keep-awake at launch.
#[tauri::command]
#[specta::specta]
pub fn set_keep_awake_restore(
    app: AppHandle,
    restore: bool,
    ctx: tauri::State<'_, AppContext>,
) -> KeepAwakeState {
    ctx.settings.update(|s| s.restore_keep_awake = restore);
    shortcuts::refresh_tray_popup(&app);
    snapshot(&ctx)
}
