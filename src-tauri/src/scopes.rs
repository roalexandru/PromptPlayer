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

/// Process id of the foreground app, for detecting focus loss mid-playback.
/// `None` is unknown, which callers treat as don't-abort.
pub fn foreground_identity() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        crate::platform::macos::nsworkspace::frontmost_app()
            .pid
            .map(|p| p as u64)
    }
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetForegroundWindow, GetWindowThreadProcessId,
        };
        // SAFETY: plain Win32 reads; the null HWND is checked before use and
        // `pid` is a valid out-pointer for the call.
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() {
            return None;
        }
        let mut pid: u32 = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        // Zero pid = lookup failed (window died); unknown, so playback runs on.
        (pid != 0).then_some(pid as u64)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn capture_macos() -> ForegroundContext {
    let snap = crate::platform::macos::nsworkspace::frontmost_app();
    ForegroundContext {
        bundle_id: snap.bundle_id,
        executable: snap.executable_path,
        // NSWorkspace exposes no window titles, so this was permanently
        // `None` — and `ScopeFilter::matches` rejects any prompt with a
        // `window-title-regex:` when the title is empty. Every title-scoped
        // prompt therefore silently never fired on macOS, including the ones
        // the library window offers to create. The AX API is the only source
        // for this; it degrades to `None` without an Accessibility grant,
        // which is the same permission the keyboard hook already needs.
        window_title: crate::accessibility::focused_window_title(),
        url: None, // browser URL via AppleScript — Phase 8 territory
    }
}

#[cfg(target_os = "windows")]
fn capture_windows() -> ForegroundContext {
    use windows::Win32::Foundation::CloseHandle;
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
                // Wrap the buffer pointer — an array reference doesn't deref to
                // the PWSTR newtype this expects.
                let pwstr = windows::core::PWSTR(buf.as_mut_ptr());
                let ok =
                    QueryFullProcessImageNameW(h, PROCESS_NAME_FORMAT::default(), pwstr, &mut sz);
                let _ = CloseHandle(h);
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

/// Best candidate among prompts sharing a trigger. §4.2 order: scope must match
/// (or be empty), then higher `priority`, then more constraints, then input order.
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
        if p.priority > best_priority || (p.priority == best_priority && spec > best_specificity) {
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
        if let Some(spec) = &self.time_of_day {
            let now = chrono::Local::now();
            let minutes = now.hour() * 60 + now.minute();
            if !matches_time_of_day(spec, minutes) {
                return false;
            }
        }
        true
    }

    #[cfg(test)]
    fn matches_at_minute(&self, ctx: &ForegroundContext, minutes: u32) -> bool {
        if !self.matches_without_time(ctx) {
            return false;
        }
        self.time_of_day
            .as_deref()
            .map(|spec| matches_time_of_day(spec, minutes))
            .unwrap_or(true)
    }

    #[cfg(test)]
    fn matches_without_time(&self, ctx: &ForegroundContext) -> bool {
        let mut clone = self.clone();
        clone.time_of_day = None;
        clone.matches(ctx)
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
        if self
            .time_of_day
            .as_deref()
            .and_then(parse_time_range)
            .is_some()
        {
            s += 1;
        }
        s
    }
}

use chrono::Timelike;

fn parse_time_range(spec: &str) -> Option<(u32, u32)> {
    let (start, end) = spec.split_once('-')?;
    Some((parse_hhmm(start.trim())?, parse_hhmm(end.trim())?))
}

fn parse_hhmm(s: &str) -> Option<u32> {
    let (hh, mm) = s.split_once(':')?;
    let hour: u32 = hh.parse().ok()?;
    let minute: u32 = mm.parse().ok()?;
    if hour < 24 && minute < 60 {
        Some(hour * 60 + minute)
    } else {
        None
    }
}

fn matches_time_of_day(spec: &str, minute: u32) -> bool {
    let Some((start, end)) = parse_time_range(spec) else {
        return false;
    };
    if start <= end {
        (start..=end).contains(&minute)
    } else {
        minute >= start || minute <= end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Windows arm is raw FFI, so it can't be exercised from a mac test
    /// runner. Pin the contract textually instead of leaving it unguarded.
    #[test]
    fn foreground_identity_reports_a_process_not_a_window() {
        const SRC: &str = include_str!("scopes.rs");
        let start = SRC
            .find("pub fn foreground_identity()")
            .expect("foreground_identity");
        let body = &SRC[start..];
        let body = &body[..body.find("\n}").expect("fn end")];

        // Match the call, not the `use` line — that import names the same
        // symbol and made an earlier version of this assertion vacuous.
        assert!(
            body.contains("GetWindowThreadProcessId(hwnd"),
            "the Windows arm must resolve the HWND to its owning process — \
             returning the raw handle aborts playback whenever the target app \
             opens a tooltip or autocomplete list"
        );
        assert!(
            !body.contains("hwnd.0 as "),
            "the raw HWND is a window id, not an application id"
        );
    }

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

    #[test]
    fn time_of_day_matches_regular_and_midnight_ranges() {
        let ctx = ForegroundContext::default();
        let day = ScopeFilter {
            time_of_day: Some("09:00-17:30".into()),
            ..Default::default()
        };
        assert!(day.matches_at_minute(&ctx, 10 * 60));
        assert!(!day.matches_at_minute(&ctx, 18 * 60));

        let overnight = ScopeFilter {
            time_of_day: Some("22:00-02:00".into()),
            ..Default::default()
        };
        assert!(overnight.matches_at_minute(&ctx, 23 * 60));
        assert!(overnight.matches_at_minute(&ctx, 60));
        assert!(!overnight.matches_at_minute(&ctx, 12 * 60));
    }

    #[test]
    fn invalid_time_of_day_does_not_match_or_count_specificity() {
        let s = ScopeFilter {
            time_of_day: Some("25:99-nope".into()),
            ..Default::default()
        };
        assert!(!s.matches_at_minute(&ForegroundContext::default(), 10 * 60));
        assert_eq!(s.specificity(), 0);
    }
}
