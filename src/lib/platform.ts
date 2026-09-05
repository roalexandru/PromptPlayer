// Platform-conditional behavior for the frontend, resolved once at import so
// callers stay synchronous — the value never changes during a session.

import { platform as tauriPlatform } from "@tauri-apps/plugin-os";

type Plat = "macos" | "windows" | "linux" | "ios" | "android";

const _plat = (() => {
  try {
    return tauriPlatform() as Plat;
  } catch {
    // Outside Tauri (Vitest/jsdom) default to macOS; tests needing Windows
    // should mock the plugin module.
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

/// Render one modifier token as a Mac symbol or Windows label. Tokens are the
/// lowercased forms used by `hotkey.rs` and `HotkeyRecorder.svelte`.
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
