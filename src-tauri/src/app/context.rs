//! Every long-lived state handle lives on one `AppContext`, shared by the
//! builder, IPC commands, shortcuts, the hook and the FireService.
//!
//! `Clone` is cheap — all fields are `Arc` — so handlers take an owned context
//! instead of juggling individual parameters.

use crate::matcher::MatcherState;
use crate::picker::{FocusStore, SearchIndex, SearchSession};
use crate::power::PowerManager;
use crate::rdp::RdpRegistry;
use crate::secure_input::SecureInputTracker;
use crate::settings::SettingsStore;
use crate::state::AppState;
use crate::store::PromptStore;
use crate::undo::UndoLog;
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

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
    /// Persisted preferences (armed restore, keep-awake duration, update nags).
    pub settings: Arc<SettingsStore>,
    /// Aggregated macOS Secure-Input activity, flushed on a timer.
    pub secure_input: Arc<SecureInputTracker>,
    /// Peak query length of the in-flight picker session.
    pub picker_search: Arc<SearchSession>,
    /// What the tray icon's attention badge is currently reporting.
    pub attention: Arc<crate::tray_icon::Attention>,
    /// Process start, for the uptime on `AppExiting`.
    pub started_at: Instant,
    /// Successful fires this run, also reported on `AppExiting`.
    pub fire_count: Arc<std::sync::atomic::AtomicUsize>,
}

impl AppContext {
    pub fn new() -> Self {
        Self::with_settings(SettingsStore::shared())
    }

    /// Build a context around an already-loaded settings store, so `run()` can
    /// read `restore_armed` before constructing `AppState`.
    pub fn with_settings(settings: Arc<SettingsStore>) -> Self {
        let s = settings.get();
        let armed = s.restore_armed && s.armed;
        Self {
            state: AppState::shared_armed(armed),
            prompts: PromptStore::new(),
            matcher: MatcherState::shared(),
            undo: Arc::new(UndoLog::new()),
            focus: FocusStore::shared(),
            search: Arc::new(Mutex::new(SearchIndex::new())),
            rdp: Arc::new(RdpRegistry::new()),
            hotkeys: Arc::new(RwLock::new(HashMap::new())),
            power: PowerManager::shared(),
            settings,
            secure_input: SecureInputTracker::shared(),
            picker_search: Arc::new(SearchSession::default()),
            attention: crate::tray_icon::Attention::shared(),
            started_at: Instant::now(),
            fire_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// True when the app was armed at boot because the user opted into
    /// restoring it — reported once on `AppStarted`.
    pub fn armed_was_restored(&self) -> bool {
        let s = self.settings.get();
        s.restore_armed && s.armed
    }
}

impl Default for AppContext {
    fn default() -> Self {
        Self::new()
    }
}
