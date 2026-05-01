//! Windows-specific hook notes.
//!
//! Phase 2 uses `rdev` cross-platform.
//! Phase 3 will swap to native `SetWindowsHookEx(WH_KEYBOARD_LL, ...)` on a
//! dedicated thread with message pump, returning non-zero from the callback to
//! suppress the commit char.
