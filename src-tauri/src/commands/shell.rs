//! Shell IPC commands — narrow surface for opening URLs in the user's
//! default browser. Webview's `window.open(url, "_blank")` is a no-op in
//! both WebView2 (Windows) and WKWebView (macOS) without explicit handler
//! plumbing, so the frontend goes through this command instead.

use crate::error::{into_ipc, AppError, IpcResult};
use url::Url;

/// Open an `http://` or `https://` URL in the user's default browser.
/// Rejects any other scheme — keeps the IPC from doubling as a generic
/// shell-execute primitive.
#[tauri::command]
#[specta::specta]
pub fn open_external(url: String) -> IpcResult<()> {
    let result = validate_external_url(&url).and_then(|safe| open_url_native(safe.as_str()));
    into_ipc(result.map_err(|e| AppError::InvalidArg(format!("open_external: {e}"))))
}

fn validate_external_url(raw: &str) -> Result<Url, String> {
    let parsed = Url::parse(raw).map_err(|e| format!("invalid URL: {e}"))?;
    match parsed.scheme() {
        "http" | "https" if parsed.host_str().is_some() => Ok(parsed),
        "http" | "https" => Err("http(s) URL must include a host".into()),
        other => Err(format!("unsupported URL scheme: {other}")),
    }
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
    use windows::core::w;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let mut wide: Vec<u16> = url.encode_utf16().collect();
    wide.push(0);
    let hinst = unsafe {
        ShellExecuteW(
            HWND::default(),
            w!("open"),
            windows::core::PCWSTR(wide.as_ptr()),
            windows::core::PCWSTR::null(),
            windows::core::PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // Per ShellExecute docs, return values <= 32 indicate failure.
    if hinst.0 as isize <= 32 {
        return Err(format!(
            "ShellExecuteW failed with code {}",
            hinst.0 as isize
        ));
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn open_url_native(url: &str) -> Result<(), String> {
    std::process::Command::new("xdg-open")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::validate_external_url;

    #[test]
    fn allows_http_and_https_with_hosts() {
        assert!(validate_external_url("https://github.com/roalexandru/PromptPlayer").is_ok());
        assert!(validate_external_url("http://example.com/path?q=1").is_ok());
    }

    #[test]
    fn rejects_non_web_or_hostless_urls() {
        assert!(validate_external_url("file:///etc/passwd").is_err());
        assert!(validate_external_url("javascript:alert(1)").is_err());
        assert!(validate_external_url("https://").is_err());
    }
}
