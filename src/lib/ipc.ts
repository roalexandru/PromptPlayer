// Thin façade over `ipc.gen.ts`, which is regenerated on every debug launch.
// Source-controlled but auto-managed — never edit it by hand.

import { commands, type Prompt, type ProfileKind, type TypingOverrides, type SearchHit, type IpcError, type UpdateInfo, type ForegroundAppInfo, type Diagnostics, type SelfTestReport, type SelfTestStep, type UiSettings, type KeepAwakeState, type Result } from "./ipc.gen";

export type { Prompt, ProfileKind, TypingOverrides, SearchHit, IpcError, UpdateInfo, ForegroundAppInfo, Diagnostics, SelfTestReport, SelfTestStep, UiSettings, KeepAwakeState };

/// Turn a specta `Result` into a throwing Promise, so call sites can `await`
/// without unpacking the `{ status, data | error }` discriminator.
async function unwrap<T>(p: Promise<Result<T, IpcError>>): Promise<T> {
  const r = await p;
  if (r.status === "ok") return r.data;
  throw r.error;
}

/// User-visible string for a thrown value. `String(e)` renders our IpcError
/// objects as "[object Object]", so prefer the structured message.
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
  resetAccessibility: () => commands.resetAccessibility(),
  // keep-awake (prevent display/screensaver/idle-sleep)
  getKeepAwake: () => commands.getKeepAwake(),
  toggleKeepAwake: (durationMins?: number) =>
    commands.toggleKeepAwake(durationMins ?? null),
  setKeepAwakeDuration: (durationMins: number) =>
    commands.setKeepAwakeDuration(durationMins),
  setKeepAwakeRestore: (restore: boolean) => commands.setKeepAwakeRestore(restore),
  // diagnostics / first-run setup
  getDiagnostics: () => commands.getDiagnostics(),
  runSelfTest: () => commands.runSelfTest(),
  selfTestType: () => commands.selfTestType(),
  openDiagnostics: () => commands.openDiagnostics(),
  getSettings: () => commands.getSettings(),
  setRestoreArmed: (restore: boolean) => commands.setRestoreArmed(restore),
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
  updaterAnnounced: (version: string) => commands.updaterAnnounced(version),
  updaterDismiss: (version: string) => commands.updaterDismiss(version),
  // library helpers (§10.2)
  captureForegroundApp: () => commands.captureForegroundApp(),
  expandPromptText: (text: string) => commands.expandPromptText(text),
  importPrompt: (sourcePath: string) => unwrap(commands.importPrompt(sourcePath)),
  exportPrompt: (promptId: string, destPath: string) =>
    unwrap(commands.exportPrompt(promptId, destPath)),
  // shell
  openExternal: (url: string) => unwrap(commands.openExternal(url)),
};
