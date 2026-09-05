// Tauri builds every window in `tauri.conf.json` at startup, `visible: false`
// included — the flag hides the OS window, it does not defer the webview or its
// navigation. So a bare `setInterval` in `onMount` starts at launch and runs
// until the process exits, for a window the user may never open. These helpers
// scope that work to the time the window is actually on screen.

import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

// Names must match the constants in `src-tauri/src/app/lifecycle.rs`.

/** This window came on screen. */
export const WINDOW_SHOWN = "window-shown";
/** This window went off screen but is still alive. */
export const WINDOW_HIDDEN = "window-hidden";
/** The prompt store changed — reload the list. */
export const LIBRARY_CHANGED = "library-changed";
/** The armed flag changed from anywhere (tray, hotkey, IPC). Payload: boolean. */
export const ARMED_CHANGED = "armed-changed";

const SHOWN = WINDOW_SHOWN;
const HIDDEN = WINDOW_HIDDEN;

/**
 * Run `tick` every `intervalMs` while this window is visible, and not at all
 * while it is hidden. Ticks once immediately on each show so the first frame
 * is current. Returns a teardown for `onDestroy`.
 */
export async function pollWhileVisible(
  tick: () => void,
  intervalMs: number,
): Promise<() => void> {
  let handle: ReturnType<typeof setInterval> | null = null;

  const start = () => {
    if (handle !== null) return;
    tick();
    handle = setInterval(tick, intervalMs);
  };
  const stop = () => {
    if (handle === null) return;
    clearInterval(handle);
    handle = null;
  };

  const unlisten: UnlistenFn[] = [
    await listen(SHOWN, start),
    await listen(HIDDEN, stop),
  ];

  // A window can already be up when the script runs — a reload, or a window
  // configured `visible: true`.
  try {
    if (await getCurrentWindow().isVisible()) start();
  } catch {
    // No permission to ask: stay idle until a show event arrives.
  }

  return () => {
    stop();
    for (const u of unlisten) u();
  };
}

/**
 * Call `onShow` each time this window comes on screen. For work that belongs
 * on a show rather than on a timer.
 */
export async function onWindowShown(
  onShow: () => void,
): Promise<UnlistenFn> {
  return listen(SHOWN, onShow);
}
