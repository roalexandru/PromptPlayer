//! Prompt CRUD IPC commands.

use crate::error::{into_ipc, AppError, IpcResult};
use crate::prompts::{library, parser, Prompt};
use crate::store::PromptStore;
use crate::typer::TypingOverrides;

#[tauri::command]
#[specta::specta]
pub fn list_prompts(store: tauri::State<'_, PromptStore>) -> Vec<Prompt> {
    store.snapshot()
}

#[tauri::command]
#[specta::specta]
pub fn library_root() -> IpcResult<String> {
    into_ipc(
        library::default_library_root()
            .map(|p| p.to_string_lossy().into_owned())
            .ok_or(AppError::LibraryRootUnresolved),
    )
}

#[tauri::command]
#[specta::specta]
pub fn save_prompt(prompt: Prompt, store: tauri::State<'_, PromptStore>) -> IpcResult<String> {
    into_ipc(store.save(&prompt).map(|p| p.to_string_lossy().into_owned()))
}

#[tauri::command]
#[specta::specta]
pub fn create_prompt(
    name: Option<String>,
    store: tauri::State<'_, PromptStore>,
) -> IpcResult<Prompt> {
    let name = name.unwrap_or_else(|| "Untitled prompt".into());
    let mut id = parser::slugify(&name);
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
    let mut path = root.join(format!("{id}.pp.md"));
    let mut n = 1;
    while path.exists() {
        n += 1;
        id = format!("{}-{n}", parser::slugify(&name));
        path = root.join(format!("{id}.pp.md"));
    }
    let prompt = Prompt {
        id: id.clone(),
        name,
        description: String::new(),
        triggers: vec![id.clone()],
        commit_char: '>',
        priority: 0,
        typing_profile: Default::default(),
        typing_overrides: TypingOverrides::default(),
        scope: None,
        filters: Vec::new(),
        hotkey: None,
        tags: Vec::new(),
        enabled: true,
        body: " your new prompt body here.".into(),
        source_path: Some(path.clone()),
    };
    into_ipc(store.save(&prompt).map(|_| prompt))
}

#[tauri::command]
#[specta::specta]
pub fn delete_prompt(prompt_id: String, store: tauri::State<'_, PromptStore>) -> IpcResult<()> {
    into_ipc(store.delete(&prompt_id))
}

#[tauri::command]
#[specta::specta]
pub fn set_prompt_enabled(
    prompt_id: String,
    enabled: bool,
    store: tauri::State<'_, PromptStore>,
) -> IpcResult<()> {
    into_ipc(store.set_enabled(&prompt_id, enabled).map(|updated| {
        tracing::info!("prompt {} → enabled={}", updated.id, updated.enabled);
    }))
}
