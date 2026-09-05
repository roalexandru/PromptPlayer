<script lang="ts">
  import { onMount } from "svelte";
  import { ipc } from "$lib/ipc";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import iconUrl from "../assets/app-icon.png";

  let version = $state("");
  // Updater state lives here too — the tray menu used to host "Check for
  // Updates", but we moved it into About so the menu can stay clean.
  type CheckState =
    | { kind: "idle" }
    | { kind: "checking" }
    | { kind: "up-to-date" }
    | { kind: "error" }
    | { kind: "available"; version: string }
    | { kind: "installing" };
  let checkState = $state<CheckState>({ kind: "idle" });

  onMount(async () => {
    try {
      version = await ipc.updaterCurrentVersion();
    } catch {
      version = "";
    }
  });

  async function checkForUpdates() {
    if (checkState.kind === "checking" || checkState.kind === "installing") return;
    checkState = { kind: "checking" };
    try {
      const info = await ipc.updaterCheck();
      checkState = info.available && info.version
        ? { kind: "available", version: info.version }
        : { kind: "up-to-date" };
    } catch {
      checkState = { kind: "error" };
    }
  }

  async function installUpdate() {
    if (checkState.kind !== "available") return;
    checkState = { kind: "installing" };
    try {
      await ipc.updaterInstall();
    } catch {
      checkState = { kind: "error" };
    }
  }

  // Both webviews no-op `window.open(url, "_blank")`, so this goes through the
  // `open_external` IPC to reach the OS handler.
  async function openProject() {
    try {
      await ipc.openExternal("https://github.com/roalexandru/PromptPlayer");
    } catch (e) {
      console.error("openExternal failed", e);
    }
  }

  async function close() {
    try {
      await getCurrentWindow().close();
    } catch {}
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape" || (e.key === "Enter" && checkState.kind !== "available")) {
      e.preventDefault();
      close();
    }
  }

  // Drive the action button label/handler from updater state — single source
  // of truth so click + render stay in sync.
  const primaryLabel = $derived.by(() => {
    switch (checkState.kind) {
      case "idle":       return "Check for Updates";
      case "checking":   return "Checking…";
      case "up-to-date": return "You're Up to Date";
      case "error":      return "Check Failed — Retry";
      case "available":  return `Install v${checkState.version}`;
      case "installing": return "Installing…";
    }
  });
  const primaryDisabled = $derived(
    checkState.kind === "checking" ||
    checkState.kind === "up-to-date" ||
    checkState.kind === "installing"
  );
  function onPrimary() {
    if (checkState.kind === "available") installUpdate();
    else if (checkState.kind === "idle" || checkState.kind === "error") checkForUpdates();
  }
</script>

<svelte:window on:keydown={onKey} />

<div class="root">
  <img class="icon" src={iconUrl} alt="Prompt Player" width="96" height="96" draggable="false" />
  <h1 class="name">Prompt Player</h1>
  <div class="version">Version {version || "—"}</div>
  <p class="tagline">Stealth keyboard utility for live demos.</p>

  <div class="actions">
    <button class="btn" onclick={onPrimary} disabled={primaryDisabled} class:accent={checkState.kind === "available"}>
      {primaryLabel}
    </button>
    <button class="btn ghost" onclick={openProject}>View on GitHub</button>
  </div>

  <div class="meta">© 2026 Alexandru Roman</div>
</div>

<style>
  :global(html), :global(body), :global(#app) {
    margin: 0;
    padding: 0;
    height: 100%;
    font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text",
      "Segoe UI", sans-serif;
    -webkit-font-smoothing: antialiased;
    background: #1c1c1e;
    color: rgba(255, 255, 255, 0.92);
    user-select: none;
    -webkit-user-select: none;
    overflow: hidden;
  }
  .root {
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    padding: 28px 32px 18px 32px;
    box-sizing: border-box;
    height: 100%;
  }
  .icon {
    width: 96px;
    height: 96px;
    /* The app icon is shipped as a light-bg squircle; on dark backdrops it
       reads cleaner with a subtle drop shadow than a hard edge. */
    filter: drop-shadow(0 2px 8px rgba(0, 0, 0, 0.4));
    margin-bottom: 14px;
    -webkit-user-drag: none;
  }
  .name {
    font-size: 18px;
    font-weight: 600;
    letter-spacing: -0.2px;
    margin: 0 0 2px 0;
  }
  .version {
    font-size: 11.5px;
    color: rgba(255, 255, 255, 0.5);
    font-feature-settings: "tnum";
    margin-bottom: 12px;
  }
  .tagline {
    font-size: 13px;
    font-weight: 500;
    margin: 0 0 18px 0;
    color: rgba(255, 255, 255, 0.85);
  }
  .actions {
    display: flex;
    gap: 8px;
    margin-bottom: 16px;
  }
  .btn {
    font: inherit;
    font-size: 12.5px;
    padding: 6px 14px;
    border-radius: 6px;
    border: 1px solid rgba(255, 255, 255, 0.18);
    background: rgba(255, 255, 255, 0.08);
    color: inherit;
    cursor: default;
    transition: background-color 100ms ease, opacity 100ms ease;
  }
  .btn:hover:not(:disabled) {
    background: rgba(255, 255, 255, 0.16);
  }
  .btn:disabled {
    opacity: 0.5;
  }
  .btn.ghost {
    background: transparent;
  }
  .btn.accent {
    background: rgba(48, 209, 88, 0.85);
    border-color: rgba(48, 209, 88, 1);
    color: #0a1f10;
    font-weight: 600;
  }
  .btn.accent:hover:not(:disabled) {
    background: rgba(48, 209, 88, 1);
  }
  .meta {
    font-size: 10.5px;
    color: rgba(255, 255, 255, 0.4);
    line-height: 1.5;
    margin-top: auto;
  }

  @media (prefers-color-scheme: light) {
    :global(html), :global(body), :global(#app) {
      background: #fafafa;
      color: rgba(0, 0, 0, 0.88);
    }
    .icon { filter: drop-shadow(0 2px 8px rgba(0, 0, 0, 0.12)); }
    .version { color: rgba(0, 0, 0, 0.5); }
    .tagline { color: rgba(0, 0, 0, 0.85); }
    .btn {
      border-color: rgba(0, 0, 0, 0.15);
      background: rgba(0, 0, 0, 0.05);
    }
    .btn:hover:not(:disabled) { background: rgba(0, 0, 0, 0.10); }
    .btn.accent { color: #fff; }
    .meta { color: rgba(0, 0, 0, 0.4); }
  }
</style>
