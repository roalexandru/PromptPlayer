//! Windows-specific keystroke synthesis notes.
//!
//! `enigo` uses `SendInput` with `KEYEVENTF_UNICODE` for non-ASCII; for Phase 1
//! we rely on it via `inject::EnigoInjector`. Refinements (scan-code preservation
//! for elevated apps via UI Access manifest) come in v2 per spec §9.2.
