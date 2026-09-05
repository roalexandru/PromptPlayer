//! Library-window helper IPC (§10.2): capture the foreground app for scoping
//! (the frontend hides itself first, so it isn't us), preview a body through
//! the real expansion pipeline, and import/export `.pp.md` files.

use crate::error::{into_ipc, AppError, IpcResult};
use crate::prompts::{
    self,
    expressions::{self, ExprContext},
    library, parser,
    placeholders::{self, PlaceholderContext},
    Prompt,
};
use crate::store::PromptStore;

/// Foreground-app identity. All optional: macOS gives bundle_id + name,
/// Windows gives executable + window_title, neither gives both.
#[derive(Debug, Clone, serde::Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ForegroundAppInfo {
    pub bundle_id: Option<String>,
    pub name: Option<String>,
    pub executable: Option<String>,
    pub window_title: Option<String>,
}

#[tauri::command]
#[specta::specta]
pub fn capture_foreground_app() -> ForegroundAppInfo {
    capture_foreground_impl()
}

#[cfg(target_os = "macos")]
fn capture_foreground_impl() -> ForegroundAppInfo {
    let snap = crate::platform::macos::nsworkspace::frontmost_app();
    // Human-readable name from the executable path, falling back to the
    // bundle id so the field is never empty.
    let name = snap
        .executable_path
        .as_deref()
        .and_then(|p| {
            std::path::Path::new(p)
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        })
        .or_else(|| snap.bundle_id.clone());
    ForegroundAppInfo {
        bundle_id: snap.bundle_id,
        name,
        executable: snap.executable_path,
        window_title: None,
    }
}

#[cfg(target_os = "windows")]
fn capture_foreground_impl() -> ForegroundAppInfo {
    use windows::Win32::Foundation::{CloseHandle, MAX_PATH};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
    };
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return ForegroundAppInfo {
                bundle_id: None,
                name: None,
                executable: None,
                window_title: None,
            };
        }
        let mut title = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut title);
        let window_title = if len > 0 {
            Some(String::from_utf16_lossy(&title[..len as usize]))
        } else {
            None
        };

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));
        let executable = if pid != 0 {
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
                .ok()
                .and_then(|h| {
                    let mut buf = [0u16; MAX_PATH as usize];
                    let mut size = buf.len() as u32;
                    let r = QueryFullProcessImageNameW(
                        h,
                        PROCESS_NAME_FORMAT(0),
                        windows::core::PWSTR(buf.as_mut_ptr()),
                        &mut size,
                    );
                    let _ = CloseHandle(h);
                    if r.is_ok() {
                        Some(String::from_utf16_lossy(&buf[..size as usize]))
                    } else {
                        None
                    }
                })
        } else {
            None
        };
        ForegroundAppInfo {
            bundle_id: None,
            name: None,
            executable,
            window_title,
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn capture_foreground_impl() -> ForegroundAppInfo {
    ForegroundAppInfo {
        bundle_id: None,
        name: None,
        executable: None,
        window_title: None,
    }
}

/// Run a string through the fire-time expansion pipeline for the editor's
/// "Test" button. Tab-stops and selection stay unresolved in a preview.
#[tauri::command]
#[specta::specta]
pub fn expand_prompt_text(text: String) -> String {
    // Same order as fire.rs: expressions (whose output may contain placeholder
    // syntax) then placeholders. Context stays empty — we're the foreground app.
    let expr_ctx = ExprContext::default();
    let after_expr = expressions::expand_expressions(&text, &expr_ctx);
    let ph_ctx = PlaceholderContext::default();
    placeholders::expand(&after_expr, &ph_ctx).text
}

/// Read and parse a `.pp.md`, then copy it into the library root with a fresh
/// id on collision. Returns the prompt so the frontend can select it.
#[tauri::command]
#[specta::specta]
pub fn import_prompt(
    source_path: String,
    store: tauri::State<'_, PromptStore>,
    ctx: tauri::State<'_, crate::app::context::AppContext>,
    app: tauri::AppHandle,
) -> IpcResult<Prompt> {
    let src = std::path::PathBuf::from(&source_path);
    let raw = match std::fs::read_to_string(&src) {
        Ok(s) => s,
        Err(e) => {
            return into_ipc(Err(AppError::Io {
                path: src,
                source: e,
            }))
        }
    };
    let parsed = match parser::parse_str(&raw, &src) {
        Ok(p) => p,
        Err(e) => return into_ipc(Err(e.into())),
    };
    let root = match library::default_library_root() {
        Some(r) => r,
        None => return into_ipc(Err(AppError::LibraryRootUnresolved)),
    };
    if let Err(e) = std::fs::create_dir_all(&root) {
        return into_ipc(Err(AppError::Io {
            path: root.clone(),
            source: e,
        }));
    }
    // Suffix `-2`, `-3`, … so an import never silently overwrites a file.
    let mut id = parsed.id.clone();
    let mut dest = root.join(format!("{id}.pp.md"));
    let mut n = 1;
    while dest.exists() {
        n += 1;
        id = format!("{}-{n}", parsed.id);
        dest = root.join(format!("{id}.pp.md"));
    }
    let prompt = Prompt {
        id: id.clone(),
        source_path: Some(dest.clone()),
        ..parsed
    };
    let result = store.save(&prompt);
    if result.is_ok() {
        crate::app::setup::reindex_after_mutation(&app, &ctx);
    }
    into_ipc(result.map(|_| prompt))
}

/// Export a prompt, re-serializing from memory rather than copying the file —
/// otherwise unsaved edits export as the older on-disk version.
#[tauri::command]
#[specta::specta]
pub fn export_prompt(
    prompt_id: String,
    dest_path: String,
    store: tauri::State<'_, PromptStore>,
) -> IpcResult<()> {
    let prompts = store.snapshot();
    let prompt = match prompts.iter().find(|p| p.id == prompt_id) {
        Some(p) => p.clone(),
        None => return into_ipc(Err(AppError::PromptNotFound(prompt_id))),
    };
    let dest = std::path::PathBuf::from(&dest_path);
    let serialized = match prompts::parser::serialize(&prompt) {
        Ok(s) => s,
        Err(e) => return into_ipc(Err(e.into())),
    };
    into_ipc(std::fs::write(&dest, serialized).map_err(|e| AppError::Io {
        path: dest,
        source: e,
    }))
}
