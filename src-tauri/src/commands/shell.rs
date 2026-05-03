//! Shell IPC commands — narrow surface for opening URLs in the user's
//! default browser. Webview's `window.open(url, "_blank")` is a no-op in
//! both WebView2 (Windows) and WKWebView (macOS) without explicit handler
//! plumbing, so the frontend goes through this command instead.

use crate::error::{into_ipc, AppError, IpcResult};

/// Open an `http://` or `https://` URL in the user's default browser.
/// Rejects any other scheme — keeps the IPC from doubling as a generic
/// shell-execute primitive.
#[tauri::command]
#[specta::specta]
pub fn open_external(url: String) -> IpcResult<()> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return into_ipc(Err(AppError::InvalidArg(format!(
            "open_external rejects non-http(s) URL: {url}"
        ))));
    }
    let result = open_url_native(&url);
    into_ipc(result.map_err(|e| AppError::InvalidArg(format!("open_external: {e}"))))
}

#[cfg(target_os = "macos")]
fn open_url_native(url: &str) -> Result<(), String> {
    std::process::Command::new("open")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(target_os = "windows")]
fn open_url_native(url: &str) -> Result<(), String> {
    // `cmd /C start "" <url>` — the empty "" is the title arg that `start`
    // would otherwise consume from the URL when the URL contains spaces.
    std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn open_url_native(url: &str) -> Result<(), String> {
    std::process::Command::new("xdg-open")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}
