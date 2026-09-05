// Thin façade over the auto-generated tauri-specta bindings.
//
// `ipc.gen.ts` is regenerated on every debug app launch (see
// `src-tauri/src/app/setup.rs::generate_typescript_bindings`).
// Treat it as source-controlled but auto-managed — do not edit by hand.

import {
  commands,
  type Prompt,
  type ProfileKind,
  type TypingOverrides,
  type SearchHit,
  type IpcError,
  type UpdateInfo,
  type ForegroundAppInfo,
  type AppConfig,
  type NewlineMode,
  type PickerDisplay,
  type SourceSpec,
  type SourceStatus,
  type SetlistEntry,
  type SaveConfigOutcome,
  type PlaybackStatus,
  type PromptStop,
  type PromptOrigin,
  type AgentImportSummary,
  type Result,
} from "./ipc.gen";

export type {
  Prompt,
  ProfileKind,
  TypingOverrides,
  SearchHit,
  IpcError,
  UpdateInfo,
  ForegroundAppInfo,
  AppConfig,
  NewlineMode,
  PickerDisplay,
  SourceSpec,
  SourceStatus,
  SetlistEntry,
  SaveConfigOutcome,
  PlaybackStatus,
  PromptStop,
  PromptOrigin,
  AgentImportSummary,
};

/** Delivery mode for a picker selection (§5.3 modifier-on-Enter). */
export type PickMode = "human" | "fast" | "paste" | "run";

/** True when a prompt came from a remote source and cannot be edited. */
export function isRemote(p: Prompt): boolean {
  // `origin` is `#[serde(default)]` on the Rust side, so an older payload (or
  // a hand-built object in a test) may omit it entirely — absent means local.
  return p.origin?.kind === "remote";
}

/** Source id a remote prompt came from, or null for a local prompt. */
export function sourceIdOf(p: Prompt): string | null {
  return p.origin?.kind === "remote" ? p.origin.source_id : null;
}

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
  // keep-awake (prevent display/screensaver/idle-sleep)
  getKeepAwake: () => commands.getKeepAwake(),
  toggleKeepAwake: () => commands.toggleKeepAwake(),
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
  pickerSelect: (
    promptId: string,
    mode: PickMode,
    answers?: Record<string, string>,
  ) => unwrap(commands.pickerSelect(promptId, mode, answers ?? null)),
  promptStops: (promptId: string) => unwrap(commands.promptStops(promptId)),
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
  // config (§7.2 promptplayer.yaml)
  getConfig: () => commands.getConfig(),
  saveConfig: (config: AppConfig) => unwrap(commands.saveConfig(config)),
  // setlist (ordered demo cues)
  getSetlist: () => commands.getSetlist(),
  setSetlist: (ids: string[]) => unwrap(commands.setSetlist(ids)),
  fireNextCue: () => unwrap(commands.fireNextCue()),
  resetSetlist: () => commands.resetSetlist(),
  // playback transport (§3.5)
  playbackStatus: () => commands.playbackStatus(),
  togglePlaybackPause: () => commands.togglePlaybackPause(),
  nudgePlaybackSpeed: (faster: boolean) => commands.nudgePlaybackSpeed(faster),
  // remote prompt sources (public GitHub repos, §7.2)
  listSources: () => commands.listSources(),
  addSource: (repo: string, gitRef?: string, subdir?: string) =>
    unwrap(commands.addSource(repo, gitRef ?? null, subdir ?? null)),
  removeSource: (sourceId: string) => unwrap(commands.removeSource(sourceId)),
  refreshSources: () => unwrap(commands.refreshSources()),
  setRemotePromptEnabled: (promptId: string, enabled: boolean) =>
    unwrap(commands.setRemotePromptEnabled(promptId, enabled)),
  forkPrompt: (promptId: string) => unwrap(commands.forkPrompt(promptId)),
  // agent-prompt import (.claude/commands, Cursor rules, …)
  importAgentPrompts: (dir: string) => unwrap(commands.importAgentPrompts(dir)),
  agentImportCandidates: () => commands.agentImportCandidates(),
  captureLastTyped: (name?: string, maxChars?: number) =>
    unwrap(commands.captureLastTyped(name ?? null, maxChars ?? null)),
  // shell
  openExternal: (url: string) => unwrap(commands.openExternal(url)),
};

/**
 * Enable or disable a prompt, routing to the right command for its origin.
 *
 * Remote prompts keep their enablement in `promptplayer.yaml` rather than in
 * the prompt file, because a source's cache is replaced wholesale on refresh.
 */
export async function setEnabled(p: Prompt, enabled: boolean): Promise<void> {
  if (isRemote(p)) {
    await ipc.setRemotePromptEnabled(p.id, enabled);
  } else {
    await ipc.setPromptEnabled(p.id, enabled);
  }
}
