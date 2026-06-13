// Thin façade over the auto-generated tauri-specta bindings.
//
// `ipc.gen.ts` is regenerated on every debug app launch (see
// `src-tauri/src/app/setup.rs::generate_typescript_bindings`).
// Treat it as source-controlled but auto-managed — do not edit by hand.

import { commands, type Prompt, type ProfileKind, type TypingOverrides, type SearchHit, type IpcError, type UpdateInfo, type ForegroundAppInfo, type Result } from "./ipc.gen";

export type { Prompt, ProfileKind, TypingOverrides, SearchHit, IpcError, UpdateInfo, ForegroundAppInfo };

/// Unwrap a tauri-specta `Result<T, IpcError>` into a Promise that throws the
/// IpcError on the error branch. Lets call sites use plain `await` syntax
/// without juggling the `{ status, data | error }` discriminator.
async function unwrap<T>(p: Promise<Result<T, IpcError>>): Promise<T> {
  const r = await p;
  if (r.status === "ok") return r.data;
  throw r.error;
}

/// Format an unknown thrown value into a user-visible string. `unwrap()`
/// throws plain `{ kind, message }` IpcError objects, which `String(e)`
/// renders as "[object Object]" — prefer the structured message when present.
export function fmtErr(e: unknown): string {
  return (e as IpcError)?.message ?? String(e);
}

export const ipc = {
  // armed
  getArmed: () => commands.getArmed(),
  toggleArmed: () => commands.toggleArmed(),
  kill: () => commands.kill(),
  isPlaying: () => commands.isPlaying(),
  isHookAlive: () => commands.isHookAlive(),
  openAccessibilitySettings: () => commands.openAccessibilitySettings(),
  // prompts
  listPrompts: () => commands.listPrompts(),
  libraryRoot: () => unwrap(commands.libraryRoot()),
  savePrompt: (prompt: Prompt) => unwrap(commands.savePrompt(prompt)),
  createPrompt: (name?: string) => unwrap(commands.createPrompt(name ?? null)),
  deletePrompt: (promptId: string) => unwrap(commands.deletePrompt(promptId)),
  setPromptEnabled: (promptId: string, enabled: boolean) =>
    unwrap(commands.setPromptEnabled(promptId, enabled)),
  setPromptPinned: (promptId: string, pinned: boolean) =>
    unwrap(commands.setPromptPinned(promptId, pinned)),
  // picker
  pickerOpen: () => unwrap(commands.pickerOpen()),
  pickerSearch: (q: string, limit?: number) =>
    commands.pickerSearch(q, limit ?? null),
  pickerSelect: (promptId: string, mode: string) =>
    unwrap(commands.pickerSelect(promptId, mode)),
  pickerDismiss: () => unwrap(commands.pickerDismiss()),
  // tray
  trayOpen: (target: "library" | "picker" | "about") =>
    unwrap(commands.trayOpen(target)),
  trayQuit: () => commands.trayQuit(),
  trayPopupHide: () => unwrap(commands.trayPopupHide()),
  trayFirePrompt: (promptId: string) => unwrap(commands.trayFirePrompt(promptId)),
  // updater
  updaterCurrentVersion: () => commands.updaterCurrentVersion(),
  updaterCheck: () => unwrap(commands.updaterCheck()),
  updaterInstall: () => unwrap(commands.updaterInstall()),
  // library helpers (§10.2)
  captureForegroundApp: () => commands.captureForegroundApp(),
  expandPromptText: (text: string) => commands.expandPromptText(text),
  importPrompt: (sourcePath: string) => unwrap(commands.importPrompt(sourcePath)),
  exportPrompt: (promptId: string, destPath: string) =>
    unwrap(commands.exportPrompt(promptId, destPath)),
  // shell
  openExternal: (url: string) => unwrap(commands.openExternal(url)),
};
