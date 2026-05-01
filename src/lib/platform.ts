// Single source of truth for platform-conditional behavior in the frontend.
//
// Resolved once at module import time via @tauri-apps/plugin-os. Synchronous
// access elsewhere in the app is the goal — async detection would force every
// caller to await, which is overkill for a value that never changes during a
// session.

import { platform as tauriPlatform } from "@tauri-apps/plugin-os";

type Plat = "macos" | "windows" | "linux" | "ios" | "android";

const _plat = (() => {
  try {
    return tauriPlatform() as Plat;
  } catch {
    // Outside a Tauri context (Vitest/jsdom), default to macOS so existing
    // dev/test paths keep working. Tests that need Windows behavior should
    // mock the plugin module.
    return "macos" as Plat;
  }
})();

export const PLATFORM: Plat = _plat;
export const IS_MAC = _plat === "macos";
export const IS_WIN = _plat === "windows";
export const IS_LINUX = _plat === "linux";

/// User-facing primary modifier label. Maps Cmd→Ctrl on Windows so that the
/// hotkey hints match what the user expects on each OS.
export const PRIMARY_MOD: "cmd" | "ctrl" = IS_MAC ? "cmd" : "ctrl";

/// Render a single normalized modifier token as the symbol (Mac) or text
/// label (Windows) the user expects. Tokens are the lowercased forms used by
/// hotkey.rs / HotkeyRecorder.svelte: `cmd`, `ctrl`, `alt`, `shift`, `win`.
export function prettyMod(token: string): string {
  const t = token.toLowerCase();
  if (IS_MAC) {
    if (t === "cmd" || t === "command" || t === "meta" || t === "super" || t === "win") return "⌘";
    if (t === "ctrl" || t === "control") return "⌃";
    if (t === "alt" || t === "option" || t === "opt") return "⌥";
    if (t === "shift") return "⇧";
    return token;
  }
  if (t === "cmd" || t === "command" || t === "meta") return "Ctrl";
  if (t === "super" || t === "win" || t === "windows") return "Win";
  if (t === "ctrl" || t === "control") return "Ctrl";
  if (t === "alt" || t === "option" || t === "opt") return "Alt";
  if (t === "shift") return "Shift";
  return token;
}
