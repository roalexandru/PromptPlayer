//! macOS-specific keystroke synthesis notes.
//!
//! `enigo` already uses `CGEventCreateKeyboardEvent`; for Phase 1 we rely on it
//! directly via `inject::EnigoInjector`. Phase 9 may add a Unicode-string fast-path
//! for runs of non-ASCII chars where `CGEventKeyboardSetUnicodeString` is faster
//! than per-key events.
