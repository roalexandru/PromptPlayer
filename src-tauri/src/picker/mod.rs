//! §5 — picker.

pub mod focus;
pub mod search;
pub mod window;

pub use focus::{FocusStore, ForegroundSnapshot, RESTORATION_DELAY, RESTORATION_TIMEOUT};
pub use search::{SearchHit, SearchIndex};
pub use window::{apply_screen_capture_exclusion, prepare_picker};

use std::sync::atomic::{AtomicUsize, Ordering};

/// How much the user typed to find a prompt, reported once on close. Only the
/// peak length is kept — the query text itself would be a content leak.
#[derive(Debug, Default)]
pub struct SearchSession {
    peak: AtomicUsize,
}

impl SearchSession {
    pub fn note(&self, len: usize) {
        self.peak.fetch_max(len, Ordering::Relaxed);
    }

    /// Peak length of the search that just ended, or `None` if the user never
    /// typed. Resets, so each open/close pair reports at most once.
    pub fn take(&self) -> Option<u8> {
        let n = self.peak.swap(0, Ordering::Relaxed);
        (n > 0).then(|| n.min(u8::MAX as usize) as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_the_peak_not_the_last_query() {
        // Backspacing to refine a search must not shrink what we report.
        let s = SearchSession::default();
        for len in [1, 2, 3, 4, 5, 2] {
            s.note(len);
        }
        assert_eq!(s.take(), Some(5));
    }

    #[test]
    fn take_resets_so_each_session_reports_once() {
        let s = SearchSession::default();
        s.note(3);
        assert_eq!(s.take(), Some(3));
        assert_eq!(s.take(), None);
    }

    #[test]
    fn a_picker_opened_and_closed_without_typing_reports_nothing() {
        assert_eq!(SearchSession::default().take(), None);
        let s = SearchSession::default();
        s.note(0);
        assert_eq!(s.take(), None);
    }

    #[test]
    fn saturates_instead_of_wrapping() {
        let s = SearchSession::default();
        s.note(9999);
        assert_eq!(s.take(), Some(u8::MAX));
    }
}
