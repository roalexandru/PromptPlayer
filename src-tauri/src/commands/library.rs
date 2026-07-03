//! Library-window helper IPC (§10.2).
//!
//! Three small commands live here, all triggered from the prompt editor:
//!  - `capture_foreground_app` — read NSWorkspace / GetForegroundWindow and
//!    return identifying metadata. The frontend hides the library window
//!    *before* calling so the captured app is the one the user actually
//!    wants to scope to (not Prompt Player itself).
//!  - `expand_prompt_text` — run the same placeholder + expression pipeline
//!    the typing engine uses, against an arbitrary string. Powers the
//!    "Test" button in the editor.
//!  - `import_prompt` / `export_prompt` — file-dialog-driven copy in/out
//!    of the library root.

use crate::error::{into_ipc, AppError, IpcResult};
use crate::prompts::{
    self,
    expressions::{self, ExprContext},
    library, parser,
    placeholders::{self, PlaceholderContext},
    Prompt,
};
use crate::store::PromptStore;

/// Identifying info for a foreground app. Every field is optional because
/// platforms surface different subsets — Mac always has bundle_id+name,
/// Windows always has executable+window_title, neither has both.
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
    // Derive a human-readable name from the executable path (e.g.,
    // "/Applications/Cursor.app/Contents/MacOS/Cursor" → "Cursor"). Using
    // the bundle ID as a fallback keeps the field non-empty.
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

/// Run a string through the same placeholder + expression pipeline used at
/// fire time. Powers the editor's "Test" button. Errors inside `${{ … }}`
/// blocks surface inline as `[expr error: …]` (consistent with how typing
/// would handle them). Tab-stops, choices, and selection are not resolved
/// here — the body is rendered as if the user had no clipboard / selection
/// context, which is the right default for an authoring preview.
#[tauri::command]
#[specta::specta]
pub fn expand_prompt_text(text: String) -> String {
    // Order matches fire.rs: expressions first (their output may contain
    // placeholder syntax), then placeholders. Context is intentionally
    // minimal — `clipboard` / `selection` / app metadata are not populated
    // because the library window is the foreground, so the values would
    // either be empty or refer to Prompt Player itself. Authors who want
    // to see those substitute can fire the prompt for real.
    let expr_ctx = ExprContext::default();
    let after_expr = expressions::expand_expressions(&text, &expr_ctx);
    let ph_ctx = PlaceholderContext::default();
    placeholders::expand(&after_expr, &ph_ctx).text
}

/// Import a `.pp.md` file: read it, parse it (validates frontmatter),
/// then copy into the library root with a fresh ID if one already exists.
/// Returns the parsed `Prompt` so the frontend can select it immediately.
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
    // Pick a non-colliding destination filename based on the imported prompt's
    // ID. Suffix `-2`, `-3`, … if needed so we never silently overwrite an
    // existing file.
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

/// Export a prompt to a user-chosen path. Re-serializes the in-memory
/// `Prompt` to `.pp.md` rather than copying the source file, so unsaved
/// edits in the library don't get exported as the older on-disk version.
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
