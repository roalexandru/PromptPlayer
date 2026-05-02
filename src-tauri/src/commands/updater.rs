//! Auto-update IPC commands (§13).
//!
//! Three commands surface to the frontend:
//!  - `updater_current_version`: synchronous — current binary version.
//!  - `updater_check`: hits the GitHub Releases endpoint, returns
//!     `UpdateInfo { available, version?, notes? }`.
//!  - `updater_install`: downloads the update, installs it, and restarts
//!     the app. Blocks until the restart begins.
//!
//! Telemetry: every successful check fires `update_check`; a successful
//! install fires `update_applied` before the process exits. No prompt
//! content or update payload is ever logged.

use crate::error::{into_ipc, AppError, IpcResult};
use crate::telemetry::{self, TelemetryEvent};
use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

/// Result of a `check()` call. `version` and `notes` are populated only
/// when an update is available; otherwise `available` is `false` and the
/// fields are `None`.
#[derive(Debug, Clone, serde::Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub available: bool,
    pub current_version: String,
    pub version: Option<String>,
    pub notes: Option<String>,
}

#[tauri::command]
#[specta::specta]
pub fn updater_current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[tauri::command]
#[specta::specta]
pub async fn updater_check(app: AppHandle) -> IpcResult<UpdateInfo> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let updater = match app.updater() {
        Ok(u) => u,
        Err(e) => {
            return into_ipc(Err(AppError::UpdaterUnavailable(e.to_string())));
        }
    };
    match updater.check().await {
        Ok(Some(update)) => {
            let info = UpdateInfo {
                available: true,
                current_version: current,
                version: Some(update.version.clone()),
                notes: update.body.clone(),
            };
            telemetry::send(
                &app,
                TelemetryEvent::UpdateCheck {
                    available: true,
                    current_version: env!("CARGO_PKG_VERSION"),
                },
            );
            Ok(info)
        }
        Ok(None) => {
            telemetry::send(
                &app,
                TelemetryEvent::UpdateCheck {
                    available: false,
                    current_version: env!("CARGO_PKG_VERSION"),
                },
            );
            Ok(UpdateInfo {
                available: false,
                current_version: current,
                version: None,
                notes: None,
            })
        }
        Err(e) => into_ipc(Err(AppError::UpdaterCheckFailed(e.to_string()))),
    }
}

#[tauri::command]
#[specta::specta]
pub async fn updater_install(app: AppHandle) -> IpcResult<()> {
    let updater = match app.updater() {
        Ok(u) => u,
        Err(e) => return into_ipc(Err(AppError::UpdaterUnavailable(e.to_string()))),
    };
    let update = match updater.check().await {
        Ok(Some(u)) => u,
        Ok(None) => return into_ipc(Err(AppError::UpdaterNoUpdateAvailable)),
        Err(e) => return into_ipc(Err(AppError::UpdaterCheckFailed(e.to_string()))),
    };
    let to_version = update.version.clone();
    if let Err(e) = update
        .download_and_install(
            |_chunk, _total| { /* no per-chunk UI for v1 — silent install */ },
            || tracing::info!("update download complete; installing"),
        )
        .await
    {
        return into_ipc(Err(AppError::UpdaterInstallFailed(e.to_string())));
    }
    telemetry::send(
        &app,
        TelemetryEvent::UpdateApplied {
            from_version: env!("CARGO_PKG_VERSION"),
            to_version: to_version.clone(),
        },
    );
    // Restart so the new bundle takes over. On macOS this re-launches the
    // .app; on Windows the MSI installer hands control back to a new exe.
    app.restart();
}
