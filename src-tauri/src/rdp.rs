//! §9.3 — RDP host-mode detection.
//!
//! When the foreground app is a recognized RDP/VM client, switch to RDP-mode
//! timing per §9.3:
//!  - Minimum inter-key delay floor: 30 ms.
//!  - Speed multiplier: ×1.3 slower than configured profile.
//!  - Disable clipboard fallback (RDP clipboard sync is unreliable).
//!  - Backspace coalescing: send single events, not bursts.
//!
//! All four are already implemented in `typer/schedule.rs` via
//! `ScheduleOptions { rdp_mode: true }`. This module is the detection layer.

use parking_lot::RwLock;
use std::sync::Arc;

/// Recognized RDP-client identifiers per §9.3 (bundle IDs on Mac, exe basenames on Win).
pub const DEFAULT_RDP_CLIENTS: &[&str] = &[
    // macOS bundle IDs
    "com.microsoft.rdc.macos",
    "com.microsoft.rdc",
    "com.parallels.desktop.console",
    "com.parallels.client",
    "com.vmware.fusion",
    "com.citrix.receiver.icaclient",
    "com.citrix.workspace",
    // Windows exe basenames
    "mstsc.exe",
    "vmconnect.exe",
    "wfica32.exe",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RdpMode {
    Off,
    HostSide,
}

/// RDP-client list. The defaults below cover the major clients; users who
/// need to add a custom client edit `promptplayer.yaml` directly (no
/// dedicated Settings UI).
#[derive(Debug, Clone)]
pub struct RdpRegistry {
    inner: Arc<RwLock<Vec<String>>>,
}

impl Default for RdpRegistry {
    fn default() -> Self {
        Self {
            inner: Arc::new(RwLock::new(
                DEFAULT_RDP_CLIENTS.iter().map(|s| s.to_string()).collect(),
            )),
        }
    }
}

impl RdpRegistry {
    /// Merge extra client identifiers from `promptplayer.yaml` (§9.3 says the
    /// recognized list is editable there). Case-insensitive; blank entries are
    /// ignored so a stray dash in the YAML can't match every app.
    pub fn extend_from_config(&self, extra: &[String]) {
        let cleaned: Vec<String> = extra
            .iter()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        if cleaned.is_empty() {
            return;
        }
        tracing::info!(
            "added {} RDP client identifier(s) from config",
            cleaned.len()
        );
        self.inner.write().extend(cleaned);
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&self, id: impl Into<String>) {
        self.inner.write().push(id.into());
    }

    pub fn replace_all(&self, ids: Vec<String>) {
        *self.inner.write() = ids;
    }

    pub fn list(&self) -> Vec<String> {
        self.inner.read().clone()
    }

    /// Decide whether the current foreground context is an RDP client.
    pub fn detect(&self, ctx: &crate::scopes::ForegroundContext) -> RdpMode {
        let ids = self.inner.read();
        let bundle = ctx.bundle_id.as_deref().unwrap_or("");
        let exe = ctx
            .executable
            .as_deref()
            .map(|s| {
                // Cross-platform basename: take after the last `/` or `\`.
                s.rsplit(['/', '\\']).next().unwrap_or(s)
            })
            .unwrap_or("");
        for needle in ids.iter() {
            // Case-insensitive: Windows executable names vary in case between
            // registry, process list, and what a user types into the config.
            if needle.eq_ignore_ascii_case(bundle) || needle.eq_ignore_ascii_case(exe) {
                return RdpMode::HostSide;
            }
        }
        RdpMode::Off
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scopes::ForegroundContext;

    #[test]
    fn extends_the_client_list_from_config() {
        let r = RdpRegistry::new();
        r.extend_from_config(&["MyRemote.exe".into(), "  ".into()]);
        let ctx = ForegroundContext {
            executable: Some("C:\\Program Files\\MyRemote.exe".into()),
            ..Default::default()
        };
        assert_eq!(r.detect(&ctx), RdpMode::HostSide);
    }

    #[test]
    fn blank_config_entries_do_not_match_everything() {
        let r = RdpRegistry::new();
        r.extend_from_config(&["".into(), "   ".into()]);
        let ctx = ForegroundContext {
            executable: Some("/Applications/Safari.app/Contents/MacOS/Safari".into()),
            ..Default::default()
        };
        assert_eq!(r.detect(&ctx), RdpMode::Off);
    }

    #[test]
    fn detects_microsoft_rdc_by_bundle() {
        let r = RdpRegistry::new();
        let ctx = ForegroundContext {
            bundle_id: Some("com.microsoft.rdc.macos".into()),
            ..Default::default()
        };
        assert_eq!(r.detect(&ctx), RdpMode::HostSide);
    }

    #[test]
    fn detects_mstsc_by_exe_basename() {
        let r = RdpRegistry::new();
        let ctx = ForegroundContext {
            executable: Some(r"C:\Windows\System32\mstsc.exe".into()),
            ..Default::default()
        };
        assert_eq!(r.detect(&ctx), RdpMode::HostSide);
    }

    #[test]
    fn ignores_unknown_app() {
        let r = RdpRegistry::new();
        let ctx = ForegroundContext {
            bundle_id: Some("com.cursor.cursor".into()),
            ..Default::default()
        };
        assert_eq!(r.detect(&ctx), RdpMode::Off);
    }
}
