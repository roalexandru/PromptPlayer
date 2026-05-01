//! §4 — per-app scopes. Phase 7 fully implements; Phase 4 stubs the type.

use serde::{Deserialize, Serialize};

/// One scope filter from §4.2: app(s), window-title regex, url regex, time-of-day.
#[derive(Debug, Clone, Default, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "kebab-case", default)]
pub struct ScopeFilter {
    pub app: Vec<String>,
    pub window_title_regex: Option<String>,
    pub url_regex: Option<String>,
    pub time_of_day: Option<String>,
}

/// Foreground app metadata captured at trigger time.
#[derive(Debug, Clone, Default)]
pub struct ForegroundContext {
    pub bundle_id: Option<String>,
    pub executable: Option<String>,
    pub window_title: Option<String>,
    pub url: Option<String>,
}

/// Capture the current foreground app's metadata.
/// Returns an empty context on platforms without an implementation.
pub fn capture_foreground_context() -> ForegroundContext {
    #[cfg(target_os = "macos")]
    {
        capture_macos()
    }
    #[cfg(target_os = "windows")]
    {
        capture_windows()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        ForegroundContext::default()
    }
}

#[cfg(target_os = "macos")]
fn capture_macos() -> ForegroundContext {
    let snap = crate::platform::macos::nsworkspace::frontmost_app();
    ForegroundContext {
        bundle_id: snap.bundle_id,
        executable: snap.executable_path,
        window_title: None, // requires AXUIElement — Phase 13
        url: None,          // browser URL via AppleScript — Phase 8 territory
    }
}

#[cfg(target_os = "windows")]
fn capture_windows() -> ForegroundContext {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
    };
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0 == 0 {
            return ForegroundContext::default();
        }
        let mut title = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut title);
        let window_title = if len > 0 {
            Some(String::from_utf16_lossy(&title[..len as usize]))
        } else {
            None
        };
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let executable = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
            .ok()
            .and_then(|h| {
                let mut buf = [0u16; 1024];
                let mut sz = buf.len() as u32;
                let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_FORMAT::default(), &mut buf, &mut sz);
                if ok.is_ok() && sz > 0 {
                    Some(String::from_utf16_lossy(&buf[..sz as usize]))
                } else {
                    None
                }
            });
        ForegroundContext {
            bundle_id: None,
            executable,
            window_title,
            url: None,
        }
    }
}

/// Pick the best prompt id from a set of candidate prompts that share a trigger.
/// Resolution order per §4.2:
///   1. Scope must match the foreground context (or be empty).
///   2. Higher `priority` wins.
///   3. Tie → more specific scope (more constraints) wins.
///   4. Final tie → first in input order.
pub fn pick_best(prompts: &[crate::prompts::Prompt], ctx: &ForegroundContext) -> Option<String> {
    let mut best: Option<&crate::prompts::Prompt> = None;
    let mut best_specificity: i64 = -1;
    let mut best_priority: i32 = i32::MIN;
    for p in prompts {
        let scope_ok = match &p.scope {
            Some(s) => s.matches(ctx),
            None => true,
        };
        if !scope_ok {
            continue;
        }
        let spec = p.scope.as_ref().map(|s| s.specificity()).unwrap_or(0) as i64;
        if p.priority > best_priority
            || (p.priority == best_priority && spec > best_specificity)
        {
            best = Some(p);
            best_priority = p.priority;
            best_specificity = spec;
        }
    }
    best.map(|p| p.id.clone())
}

impl ScopeFilter {
    /// Returns true if `ctx` matches this scope. Empty scope matches everything.
    pub fn matches(&self, ctx: &ForegroundContext) -> bool {
        if !self.app.is_empty() {
            let any_app = self.app.iter().any(|pat| {
                if let Some(b) = &ctx.bundle_id {
                    if pat == b {
                        return true;
                    }
                }
                if let Some(e) = &ctx.executable {
                    if pat == e {
                        return true;
                    }
                }
                false
            });
            if !any_app {
                return false;
            }
        }
        if let Some(re) = &self.window_title_regex {
            let title = ctx.window_title.as_deref().unwrap_or("");
            match regex::Regex::new(re) {
                Ok(r) => {
                    if !r.is_match(title) {
                        return false;
                    }
                }
                Err(_) => return false,
            }
        }
        if let Some(re) = &self.url_regex {
            let url = ctx.url.as_deref().unwrap_or("");
            match regex::Regex::new(re) {
                Ok(r) => {
                    if !r.is_match(url) {
                        return false;
                    }
                }
                Err(_) => return false,
            }
        }
        true
    }

    /// Specificity — number of constraints. Higher = more specific.
    pub fn specificity(&self) -> u32 {
        let mut s = 0;
        if !self.app.is_empty() {
            s += 1;
        }
        if self.window_title_regex.is_some() {
            s += 1;
        }
        if self.url_regex.is_some() {
            s += 1;
        }
        if self.time_of_day.is_some() {
            s += 1;
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_scope_matches_everything() {
        let s = ScopeFilter::default();
        let ctx = ForegroundContext::default();
        assert!(s.matches(&ctx));
    }

    #[test]
    fn app_filter_matches_bundle_id() {
        let s = ScopeFilter {
            app: vec!["com.cursor.cursor".into()],
            ..Default::default()
        };
        let ctx = ForegroundContext {
            bundle_id: Some("com.cursor.cursor".into()),
            ..Default::default()
        };
        assert!(s.matches(&ctx));
    }

    #[test]
    fn app_filter_misses_other_app() {
        let s = ScopeFilter {
            app: vec!["com.cursor.cursor".into()],
            ..Default::default()
        };
        let ctx = ForegroundContext {
            bundle_id: Some("com.slack.app".into()),
            ..Default::default()
        };
        assert!(!s.matches(&ctx));
    }

    #[test]
    fn specificity_counts_constraints() {
        let s = ScopeFilter {
            app: vec!["x".into()],
            window_title_regex: Some(".*chat.*".into()),
            ..Default::default()
        };
        assert_eq!(s.specificity(), 2);
    }
}
