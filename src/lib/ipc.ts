// Typed wrappers around Tauri commands. Mirrors `src-tauri/src/main.rs` IPC.

import { invoke } from "@tauri-apps/api/core";

export type ProfileKind =
  | "sales-engineer"
  | "fast-presenter"
  | "thoughtful-ceo"
  | "custom";

export interface TypingOverrides {
  "iki-median-ms"?: number | null;
  "typo-rate"?: number | null;
  "pause-variance-scale"?: number | null;
  "burst-enabled"?: boolean | null;
  "typos-enabled"?: boolean | null;
  "pre-submit-pause-enabled"?: boolean | null;
  "send-final-enter"?: boolean | null;
}

export interface Prompt {
  id: string;
  name: string;
  description: string;
  triggers: string[];
  commit_char: string;
  priority: number;
  typing_profile: ProfileKind;
  typing_overrides: TypingOverrides;
  scope: unknown;
  filters: string[];
  hotkey: string | null;
  tags: string[];
  enabled: boolean;
  body: string;
}

export const ipc = {
  getArmed: () => invoke<boolean>("ipc_get_armed"),
  toggleArmed: () => invoke<boolean>("ipc_toggle_armed"),
  kill: () => invoke<void>("ipc_kill"),
  listPrompts: () => invoke<Prompt[]>("ipc_list_prompts"),
  libraryRoot: () => invoke<string>("ipc_library_root"),
  savePrompt: (prompt: Prompt) =>
    invoke<string>("ipc_save_prompt", { prompt }),
  createPrompt: (name?: string) =>
    invoke<Prompt>("ipc_create_prompt", { name: name ?? null }),
  deletePrompt: (promptId: string) =>
    invoke<void>("ipc_delete_prompt", { promptId }),
  setPromptEnabled: (promptId: string, enabled: boolean) =>
    invoke<void>("ipc_set_prompt_enabled", { promptId, enabled }),
  trayOpen: (label: "library" | "picker" | "settings" | "about") =>
    invoke<void>("ipc_tray_open", { target: label }),
  trayQuit: () => invoke<void>("ipc_tray_quit"),
  trayPopupHide: () => invoke<void>("ipc_tray_popup_hide"),
  pickerDismiss: () => invoke<void>("ipc_picker_dismiss"),
};
