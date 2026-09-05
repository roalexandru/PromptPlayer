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
    self, agent_import,
    expressions::{self, ExprContext},
    library, parser,
    placeholders::{self, PlaceholderContext},
    Prompt,
};
use crate::store::PromptStore;
use crate::telemetry::{self, TelemetryEvent};

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

/// Result of scanning a directory for agent prompt files.
#[derive(Debug, Clone, serde::Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentImportSummary {
    /// Prompts written into the library.
    pub imported: Vec<Prompt>,
    /// Files recognised but skipped because an identical trigger already
    /// existed and the body matched — re-importing the same project is a
    /// no-op rather than a pile of `-2` duplicates.
    pub skipped: u32,
    /// Per-file problems. Never fatal; one bad file must not abort an import.
    pub errors: Vec<String>,
}

/// Import every agent prompt file under `dir` (`.claude/commands`, Claude Code
/// skills, Cursor rules, Continue/Copilot prompt files).
///
/// This is the shortest path from "I already have thirty slash commands" to
/// "I can fire any of them into any editor", and the reason the `.pp.md`
/// format was chosen to look like theirs in the first place (§7.3).
#[tauri::command]
#[specta::specta]
pub fn import_agent_prompts(
    dir: String,
    store: tauri::State<'_, PromptStore>,
    ctx: tauri::State<'_, crate::app::context::AppContext>,
    app: tauri::AppHandle,
) -> IpcResult<AgentImportSummary> {
    let root = std::path::PathBuf::from(&dir);
    let (found, mut errors) = agent_import::scan(&root);
    let mut imported = Vec::new();
    let mut skipped = 0u32;
    let mut by_format: std::collections::HashMap<&'static str, u16> =
        std::collections::HashMap::new();

    for discovered in found {
        let mut prompt = discovered.prompt;
        // An identical prompt already imported means this is a re-scan.
        if let Some(existing) = store.find(&prompt.id) {
            if existing.body == prompt.body {
                skipped += 1;
                continue;
            }
        }
        // Otherwise pick a free id and a free trigger. Both matter: the store
        // rejects a duplicate trigger outright (§2.2), and silently renaming
        // only the id would leave two prompts fighting over one trigger.
        let base_id = prompt.id.clone();
        let base_trigger = prompt.triggers.first().cloned().unwrap_or(base_id.clone());
        let mut n = 1;
        while store.find(&prompt.id).is_some() || store.validate_unique_triggers(&prompt).is_err() {
            n += 1;
            if n > 50 {
                errors.push(format!(
                    "{}: could not find a free trigger for {base_trigger}",
                    discovered.source.display()
                ));
                break;
            }
            prompt.id = format!("{base_id}-{n}");
            prompt.triggers = vec![format!("{base_trigger}-{n}")];
        }
        if n > 50 {
            continue;
        }
        prompt.source_path = None; // let the store place it in the library root
        match store.save(&prompt) {
            Ok(_) => {
                *by_format.entry(discovered.format.label()).or_insert(0) += 1;
                imported.push(prompt);
            }
            Err(e) => errors.push(format!("{}: {e}", discovered.source.display())),
        }
    }

    if !imported.is_empty() {
        crate::app::setup::reindex_after_mutation(&app, &ctx);
    }
    for (kind, count) in by_format {
        telemetry::send(&app, TelemetryEvent::AgentPromptsImported { kind, count });
    }
    into_ipc(Ok(AgentImportSummary {
        imported,
        skipped,
        errors,
    }))
}

/// Directories worth offering as import candidates: the user's home and, when
/// one can be resolved, the repo they are demoing from.
#[tauri::command]
#[specta::specta]
pub fn agent_import_candidates(
    ctx: tauri::State<'_, crate::app::context::AppContext>,
) -> Vec<String> {
    let mut out = Vec::new();
    for hint in ctx.config.get().repo_hints {
        out.push(hint);
    }
    if let Some(home) = dirs::home_dir() {
        out.push(home.to_string_lossy().into_owned());
    }
    out.dedup();
    out
}

/// Turn the last thing the user typed into a stored prompt.
///
/// The keyboard hook already keeps a ring of recent keystrokes for trigger
/// matching, so the text is in memory — the fastest possible path from "that
/// prompt worked" to "that prompt is saved". Only ever reads keystrokes the
/// hook already observed; nothing new is captured.
#[tauri::command]
#[specta::specta]
pub fn capture_last_typed(
    name: Option<String>,
    max_chars: Option<u32>,
    store: tauri::State<'_, PromptStore>,
    ctx: tauri::State<'_, crate::app::context::AppContext>,
    app: tauri::AppHandle,
) -> IpcResult<Prompt> {
    const CAPTURE_WINDOW: std::time::Duration = std::time::Duration::from_secs(5 * 60);
    let limit = max_chars.unwrap_or(240).clamp(1, 4096) as usize;
    let text = ctx.matcher.recent_text(limit, CAPTURE_WINDOW);
    let body = text.trim();
    if body.is_empty() {
        return into_ipc(Err(AppError::InvalidArg(
            "nothing recent to capture — type the prompt first, then save it".into(),
        )));
    }
    let name = name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| first_words(body, 6));
    let mut id = parser::slugify(&name);
    if id.is_empty() {
        id = "captured".into();
    }
    let mut prompt = Prompt {
        id: id.clone(),
        name,
        description: "Captured from recent keystrokes".into(),
        triggers: vec![id.clone()],
        commit_char: '>',
        priority: 0,
        typing_profile: Default::default(),
        typing_overrides: Default::default(),
        scope: None,
        filters: Vec::new(),
        hotkey: None,
        tags: vec!["captured".into()],
        // Off until reviewed: a capture is a rough draft, and an armed app
        // firing a half-remembered body mid-demo is the wrong default.
        enabled: false,
        pinned: false,
        newline_mode: None,
        origin: Default::default(),
        body: body.to_string(),
        source_path: None,
    };
    // A capture is created disabled, and trigger validation only considers
    // active prompts — so check against an enabled copy. Otherwise the trigger
    // looks free now and the user's later "enable" is refused instead.
    let free = |p: &Prompt, store: &PromptStore| {
        store.find(&p.id).is_none()
            && store
                .validate_unique_triggers(&Prompt {
                    enabled: true,
                    ..p.clone()
                })
                .is_ok()
    };
    let mut n = 1;
    while !free(&prompt, &store) {
        n += 1;
        if n > 50 {
            return into_ipc(Err(AppError::InvalidArg(
                "could not find a free trigger for the captured prompt".into(),
            )));
        }
        prompt.id = format!("{id}-{n}");
        prompt.triggers = vec![format!("{id}-{n}")];
    }
    let result = store.save(&prompt);
    if result.is_ok() {
        crate::app::setup::reindex_after_mutation(&app, &ctx);
    }
    into_ipc(result.map(|_| prompt))
}

/// First `n` words of `text`, for naming a captured prompt.
fn first_words(text: &str, n: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().take(n).collect();
    if words.is_empty() {
        return "Captured prompt".into();
    }
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompts::agent_import::AgentFormat;

    #[test]
    fn names_a_capture_from_its_first_words() {
        assert_eq!(
            first_words("Review this diff and tell me what is risky about it", 6),
            "Review this diff and tell me"
        );
    }

    #[test]
    fn naming_handles_short_and_empty_input() {
        assert_eq!(first_words("hi", 6), "hi");
        assert_eq!(first_words("   ", 6), "Captured prompt");
    }

    #[test]
    fn agent_format_labels_are_stable() {
        // These labels reach telemetry and the imported-prompt tags, so a
        // rename is a breaking change worth noticing here.
        assert_eq!(AgentFormat::ClaudeCommand.label(), "claude-command");
        assert_eq!(AgentFormat::CursorRule.label(), "cursor-rule");
    }
}
