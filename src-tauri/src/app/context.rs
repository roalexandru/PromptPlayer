//! Centralized runtime state shared between the Tauri builder, IPC commands,
//! shortcut handlers, the keyboard hook, and the FireService.
//!
//! Every long-lived state handle the app needs lives on a single `AppContext`.
//! It implements `Clone` (cheap — all fields are `Arc`-shared) so handlers
//! that need state can take an owned context rather than juggling state
//! parameters individually.

use crate::matcher::MatcherState;
use crate::picker::{FocusStore, SearchIndex};
use crate::power::PowerManager;
use crate::rdp::RdpRegistry;
use crate::state::AppState;
use crate::store::PromptStore;
use crate::undo::UndoLog;
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::sync::Arc;

/// Map: hotkey string (as authored in YAML, e.g. `"cmd+shift+1"`) → `prompt_id`.
pub type PromptHotkeyMap = Arc<RwLock<HashMap<String, String>>>;

#[derive(Clone)]
pub struct AppContext {
    pub state: Arc<AppState>,
    pub prompts: PromptStore,
    pub matcher: Arc<MatcherState>,
    pub undo: Arc<UndoLog>,
    pub focus: Arc<FocusStore>,
    pub search: Arc<Mutex<SearchIndex>>,
    pub rdp: Arc<RdpRegistry>,
    pub hotkeys: PromptHotkeyMap,
    /// "Keep Awake" controller — inhibits display/screensaver/idle-sleep.
    pub power: Arc<PowerManager>,
}

impl AppContext {
    pub fn new() -> Self {
        Self {
            state: AppState::shared(),
            prompts: PromptStore::new(),
            matcher: MatcherState::shared(),
            undo: Arc::new(UndoLog::new()),
            focus: FocusStore::shared(),
            search: Arc::new(Mutex::new(SearchIndex::new())),
            rdp: Arc::new(RdpRegistry::new()),
            hotkeys: Arc::new(RwLock::new(HashMap::new())),
            power: PowerManager::shared(),
        }
    }
}

impl Default for AppContext {
    fn default() -> Self {
        Self::new()
    }
}
