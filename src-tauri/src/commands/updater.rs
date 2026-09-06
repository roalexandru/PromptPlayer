//! Auto-update IPC (§13). Failed checks report too — a broken updater used to
//! look identical to an up-to-date machine — and the install path flushes
//! before handing off, since on Windows the installer never gives us back.

use crate::app::context::AppContext;
use crate::error::{into_ipc, AppError, IpcResult};
use crate::telemetry::{self, TelemetryEvent, UpdateFailStage};
use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

/// Result of a `check()`. `version` and `notes` are set only when an update is
/// actually available.
#[derive(Debug, Clone, serde::Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub available: bool,
    pub current_version: String,
    pub version: Option<String>,
    pub notes: Option<String>,
    /// True when the user has already dismissed this exact version.
    pub dismissed: bool,
}

#[tauri::command]
#[specta::specta]
pub fn updater_current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[tauri::command]
#[specta::specta]
pub async fn updater_check(
    app: AppHandle,
    ctx: tauri::State<'_, AppContext>,
) -> IpcResult<UpdateInfo> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let updater = match app.updater() {
        Ok(u) => u,
        Err(e) => {
            telemetry::send(
                &app,
                TelemetryEvent::UpdateCheckFailed {
                    stage: UpdateFailStage::Unavailable,
                },
            );
            return into_ipc(Err(AppError::UpdaterUnavailable(e.to_string())));
        }
    };
    match updater.check().await {
        Ok(Some(update)) => {
            telemetry::send(&app, TelemetryEvent::UpdateCheck { available: true });
            let dismissed = ctx.settings.get().dismissed_update.as_deref() == Some(&update.version);
            Ok(UpdateInfo {
                available: true,
                current_version: current,
                version: Some(update.version.clone()),
                notes: update.body.clone(),
                dismissed,
            })
        }
        Ok(None) => {
            // A manual check is a deliberate user action, so it always
            // reports — only the 6-hourly poller is throttled.
            telemetry::send(&app, TelemetryEvent::UpdateCheck { available: false });
            Ok(UpdateInfo {
                available: false,
                current_version: current,
                version: None,
                notes: None,
                dismissed: false,
            })
        }
        Err(e) => {
            telemetry::send(
                &app,
                TelemetryEvent::UpdateCheckFailed {
                    stage: UpdateFailStage::Check,
                },
            );
            into_ipc(Err(AppError::UpdaterCheckFailed(e.to_string())))
        }
    }
}

/// The user saw an "Install update" affordance. Once per version, so a
/// long-ignored update doesn't inflate the count on every poll.
#[tauri::command]
#[specta::specta]
pub fn updater_announced(app: AppHandle, version: String, ctx: tauri::State<'_, AppContext>) {
    let already = ctx.settings.get().announced_update.as_deref() == Some(&version);
    if already {
        return;
    }
    ctx.settings.update(|s| s.announced_update = Some(version));
    telemetry::send(&app, TelemetryEvent::UpdateAvailableShown);
}

/// User dismissed the update. Clears the tray badge and suppresses the nag
/// until a newer version lands.
#[tauri::command]
#[specta::specta]
pub fn updater_dismiss(app: AppHandle, version: String, ctx: tauri::State<'_, AppContext>) {
    dismiss_version(&app, &ctx, version);
}

/// The one dismiss sequence — parked version, settings, badge, telemetry.
/// Shared so the Windows native menu can't reimplement three of the four.
pub fn dismiss_version(app: &AppHandle, ctx: &AppContext, version: String) {
    if ctx.pending_update.read().as_deref() == Some(version.as_str()) {
        *ctx.pending_update.write() = None;
    }
    ctx.settings.update(|s| s.dismissed_update = Some(version));
    if ctx.attention.set_update(false) {
        crate::tray_icon::refresh(app);
    }
    telemetry::send(app, TelemetryEvent::UpdateDismissed);
}

#[tauri::command]
#[specta::specta]
pub async fn updater_install(app: AppHandle) -> IpcResult<()> {
    install_now(&app).await
}

/// The one install sequence — check, flush, download, restart. Shared so the
/// Windows native tray menu goes through the same reporting as the popover.
pub async fn install_now(app: &AppHandle) -> IpcResult<()> {
    let app = app.clone();
    let updater = match app.updater() {
        Ok(u) => u,
        Err(e) => {
            telemetry::send(
                &app,
                TelemetryEvent::UpdateCheckFailed {
                    stage: UpdateFailStage::Unavailable,
                },
            );
            return into_ipc(Err(AppError::UpdaterUnavailable(e.to_string())));
        }
    };
    let update = match updater.check().await {
        Ok(Some(u)) => u,
        Ok(None) => return into_ipc(Err(AppError::UpdaterNoUpdateAvailable)),
        Err(e) => {
            telemetry::send(
                &app,
                TelemetryEvent::UpdateCheckFailed {
                    stage: UpdateFailStage::Check,
                },
            );
            return into_ipc(Err(AppError::UpdaterCheckFailed(e.to_string())));
        }
    };
    let to_version = update.version.clone();
    // Drain BEFORE the handoff: on Windows the installer kills us inside
    // `download_and_install`, so anything queued after this is lost.
    telemetry::send_and_flush(&app, TelemetryEvent::UpdateInstallStarted);
    if let Err(e) = update
        .download_and_install(
            |_chunk, _total| { /* no per-chunk UI for v1 — silent install */ },
            || tracing::info!("update download complete; installing"),
        )
        .await
    {
        telemetry::send_and_flush(&app, TelemetryEvent::UpdateInstallFailed);
        return into_ipc(Err(AppError::UpdaterInstallFailed(e.to_string())));
    }
    telemetry::send_and_flush(
        &app,
        TelemetryEvent::UpdateApplied {
            to_version: truncate_version(&to_version),
        },
    );
    // Restart so the new bundle takes over. On macOS this re-launches the
    // .app; on Windows the MSI installer hands control back to a new exe.
    app.restart();
}

/// Keep the version string inside the §12 payload budget even if the manifest
/// carries something unexpectedly long.
fn truncate_version(v: &str) -> String {
    v.chars().take(24).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_truncated_to_fit_the_payload_rule() {
        assert_eq!(truncate_version("0.1.9"), "0.1.9");
        let long = "9".repeat(64);
        assert_eq!(truncate_version(&long).len(), 24);
        assert!(truncate_version(&long).len() < 32);
    }
}
