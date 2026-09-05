<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { ipc, fmtErr } from "$lib/ipc";
  import { IS_MAC } from "$lib/platform";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { pollWhileVisible } from "$lib/events";
  import type { Diagnostics, SelfTestReport, UiSettings } from "$lib/ipc";

  let diag = $state<Diagnostics | null>(null);
  let settings = $state<UiSettings | null>(null);
  let report = $state<SelfTestReport | null>(null);
  let busy = $state(false);
  let probeValue = $state("");
  let probeEl = $state<HTMLInputElement | null>(null);
  let roundtrip = $state<"idle" | "waiting" | "pass" | "fail">("idle");
  let closeError = $state<string | null>(null);
  let stopPolling: (() => void) | null = null;

  async function refresh() {
    try {
      diag = await ipc.getDiagnostics();
      settings = await ipc.getSettings();
    } catch (e) {
      console.error("diagnostics refresh failed", e);
    }
  }

  onMount(async () => {
    // The whole point is watching status change while the user fixes it in
    // System Settings, so poll — but only while this window is on screen.
    // Tauri creates it at launch even though it starts hidden.
    stopPolling = await pollWhileVisible(refresh, 2000);
  });
  onDestroy(() => {
    stopPolling?.();
  });

  async function runSelfTest() {
    busy = true;
    try {
      report = await ipc.runSelfTest();
    } finally {
      busy = false;
    }
    await refresh();
  }

  // Real end-to-end check: focus our own field, ask the backend to synthesize
  // keystrokes, and compare. Every status row above this is only a read.
  async function runRoundtrip() {
    if (!report) return;
    probeValue = "";
    roundtrip = "waiting";
    probeEl?.focus();
    await ipc.selfTestType();
    const expected = report.probe;
    const deadline = Date.now() + 4000;
    while (Date.now() < deadline) {
      if (probeValue.trim() === expected) {
        roundtrip = "pass";
        return;
      }
      await new Promise((r) => setTimeout(r, 100));
    }
    roundtrip = probeValue.trim() === expected ? "pass" : "fail";
  }

  async function grant() {
    await ipc.openAccessibilitySettings();
  }

  async function resetPermission() {
    busy = true;
    try {
      await ipc.resetAccessibility();
    } finally {
      busy = false;
    }
    await refresh();
  }

  async function toggleRestoreArmed(e: Event) {
    const on = (e.target as HTMLInputElement).checked;
    settings = await ipc.setRestoreArmed(on);
  }

  async function toggleRestoreKeepAwake(e: Event) {
    const on = (e.target as HTMLInputElement).checked;
    await ipc.setKeepAwakeRestore(on);
    settings = await ipc.getSettings();
  }

  async function close() {
    // Don't swallow this: a denied `core:window:allow-close` looks exactly
    // like a dead button, which is how the missing capability went unnoticed.
    try {
      await getCurrentWindow().close();
    } catch (e) {
      console.error("diagnostics close failed", e);
      closeError = fmtErr(e);
    }
  }

  function onKey(e: KeyboardEvent) {
    // Escape closes, unless the user is mid-roundtrip in the probe field.
    if (e.key === "Escape" && roundtrip !== "waiting") {
      e.preventDefault();
      close();
    }
  }

  const okIcon = (v: boolean) => (v ? "✓" : "✕");
</script>

<svelte:window on:keydown={onKey} />

<div class="root">
  <h1>Diagnostics</h1>

  {#if diag}
    {#if diag.needsAttention}
      <div class="banner">
        <strong>Triggers can't fire.</strong>
        {#if !diag.accessibilityTrusted}
          Prompt Player needs Accessibility permission to watch for your trigger words.
        {:else}
          Accessibility is granted but the keyboard hook didn't install.
        {/if}
        <div class="banner-actions">
          {#if IS_MAC}
            <button class="btn accent" onclick={grant}>Open Accessibility Settings</button>
            <button class="btn" onclick={resetPermission} disabled={busy}>Reset &amp; Reapprove</button>
          {/if}
        </div>
        {#if IS_MAC}
          <p class="hint">
            If the checkbox is already on, use Reset &amp; Reapprove — an unsigned
            update invalidates the old approval while leaving it ticked.
          </p>
        {/if}
      </div>
    {/if}

    <section>
      <h2>Status</h2>
      <div class="rows">
        {#if IS_MAC}
          <div class="row" class:bad={!diag.accessibilityTrusted}>
            <span class="mark">{okIcon(diag.accessibilityTrusted)}</span>
            <span class="label">Accessibility permission</span>
            <span class="value">{diag.accessibilityTrusted ? "Granted" : "Not granted"}</span>
          </div>
        {/if}
        <div class="row" class:bad={!diag.hookAlive}>
          <span class="mark">{okIcon(diag.hookAlive)}</span>
          <span class="label">Keyboard hook</span>
          <span class="value">{diag.hookAlive ? "Listening" : "Not installed"}</span>
        </div>
        {#if IS_MAC}
          <div class="row" class:warn={diag.secureInputActive}>
            <span class="mark">{diag.secureInputActive ? "!" : "✓"}</span>
            <span class="label">Secure Input</span>
            <span class="value">{diag.secureInputActive ? "Active — triggers gated" : "Clear"}</span>
          </div>
        {/if}
        <div class="row" class:warn={diag.captureDegraded}>
          <span class="mark">{diag.captureDegraded ? "!" : "✓"}</span>
          <span class="label">Hidden from screen capture</span>
          <span class="value">
            {diag.captureDegraded ? "Not fully — picker may be visible" : "Yes"}
          </span>
        </div>
        <div class="row">
          <span class="mark">{diag.armed ? "●" : "○"}</span>
          <span class="label">Armed</span>
          <span class="value">{diag.armed ? "Yes" : "No"}</span>
        </div>
        <div class="row" class:bad={diag.triggers === 0}>
          <span class="mark">{okIcon(diag.triggers > 0)}</span>
          <span class="label">Triggers indexed</span>
          <span class="value">{diag.triggers} from {diag.enabledPrompts}/{diag.prompts} prompts</span>
        </div>
        <div class="row">
          <span class="mark">{diag.hotkeys > 0 ? "✓" : "○"}</span>
          <span class="label">Prompt hotkeys</span>
          <span class="value">{diag.hotkeys} registered</span>
        </div>
        <div class="row">
          <span class="mark">{diag.keepAwake ? "●" : "○"}</span>
          <span class="label">Keep Awake</span>
          <span class="value">{diag.keepAwake ? "On" : "Off"}</span>
        </div>
      </div>
    </section>

    <section>
      <h2>Self test</h2>
      <button class="btn" onclick={runSelfTest} disabled={busy}>
        {busy ? "Running…" : "Run self test"}
      </button>
      {#if report}
        <div class="rows">
          {#each report.steps as step}
            <div class="row" class:bad={!step.passed}>
              <span class="mark">{okIcon(step.passed)}</span>
              <span class="label">{step.name}</span>
              <span class="value">{step.detail}</span>
            </div>
          {/each}
        </div>
        <div class="probe">
          <label for="probe">Type test — click Run, then don't touch the keyboard:</label>
          <div class="probe-row">
            <input
              id="probe"
              bind:this={probeEl}
              bind:value={probeValue}
              placeholder="keystrokes land here"
              spellcheck="false"
              autocomplete="off"
            />
            <button class="btn" onclick={runRoundtrip} disabled={roundtrip === "waiting"}>
              {roundtrip === "waiting" ? "Typing…" : "Run"}
            </button>
          </div>
          {#if roundtrip === "pass"}
            <div class="result good">Keystroke delivery works end to end.</div>
          {:else if roundtrip === "fail"}
            <div class="result bad-text">
              Nothing arrived — synthesis is blocked. On macOS this is almost
              always Accessibility.
            </div>
          {/if}
        </div>
      {/if}
    </section>

    <section>
      <h2>Behaviour</h2>
      <label class="check">
        <input
          type="checkbox"
          checked={settings?.restoreArmed ?? false}
          onchange={toggleRestoreArmed}
        />
        Stay armed between launches
      </label>
      <p class="hint">Off by default — Prompt Player starts disarmed every launch.</p>

      <label class="check">
        <input
          type="checkbox"
          checked={settings?.restoreKeepAwake ?? false}
          onchange={toggleRestoreKeepAwake}
        />
        Restore Keep Awake between launches
      </label>
      <p class="hint">
        The auto-off timer restarts each launch, so this can't resurrect a
        multi-day session.
      </p>
    </section>

    <section>
      <h2>Details</h2>
      <div class="kv"><span>Version</span><code>{diag.version}</code></div>
      <div class="kv"><span>Prompts</span><code>{diag.libraryRoot}</code></div>
      <div class="kv"><span>Logs</span><code>{diag.logDir}</code></div>
    </section>
    {#if closeError}
      <p class="hint error">Couldn't close this window: {closeError}</p>
    {/if}
  {:else}
    <p class="hint">Loading…</p>
  {/if}
</div>

<style>
  :global(html), :global(body), :global(#app) {
    margin: 0;
    padding: 0;
    height: 100%;
    font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", "Segoe UI", sans-serif;
    -webkit-font-smoothing: antialiased;
    background: #1c1c1e;
    color: rgba(255, 255, 255, 0.92);
    overflow-y: auto;
  }
  .root {
    padding: 22px 24px 28px 24px;
    box-sizing: border-box;
  }
  h1 {
    font-size: 17px;
    font-weight: 600;
    margin: 0 0 16px 0;
  }
  h2 {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.6px;
    color: rgba(255, 255, 255, 0.45);
    margin: 0 0 8px 0;
  }
  section {
    margin-bottom: 22px;
  }
  .banner {
    background: rgba(255, 159, 10, 0.14);
    border: 1px solid rgba(255, 159, 10, 0.4);
    border-radius: 8px;
    padding: 12px 14px;
    font-size: 12.5px;
    line-height: 1.45;
    margin-bottom: 20px;
  }
  .banner-actions {
    display: flex;
    gap: 8px;
    margin-top: 10px;
    flex-wrap: wrap;
  }
  .rows {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-top: 8px;
  }
  .row {
    display: grid;
    grid-template-columns: 18px 1fr auto;
    align-items: baseline;
    gap: 8px;
    font-size: 12.5px;
    padding: 5px 8px;
    border-radius: 5px;
    background: rgba(255, 255, 255, 0.04);
  }
  .row.bad {
    background: rgba(255, 69, 58, 0.14);
  }
  .row.warn {
    background: rgba(255, 159, 10, 0.12);
  }
  .mark {
    text-align: center;
    opacity: 0.75;
    font-weight: 600;
  }
  .row.bad .mark {
    color: #ff6961;
    opacity: 1;
  }
  .value {
    color: rgba(255, 255, 255, 0.55);
    font-size: 11.5px;
    text-align: right;
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
  }
  .btn:hover:not(:disabled) {
    background: rgba(255, 255, 255, 0.16);
  }
  .btn:disabled {
    opacity: 0.5;
  }
  .btn.accent {
    background: rgba(48, 209, 88, 0.85);
    border-color: rgba(48, 209, 88, 1);
    color: #0a1f10;
    font-weight: 600;
  }
  .probe {
    margin-top: 12px;
  }
  .probe label {
    display: block;
    font-size: 11.5px;
    color: rgba(255, 255, 255, 0.55);
    margin-bottom: 6px;
  }
  .probe-row {
    display: flex;
    gap: 8px;
  }
  .probe input {
    flex: 1;
    font: inherit;
    font-size: 12.5px;
    padding: 6px 10px;
    border-radius: 6px;
    border: 1px solid rgba(255, 255, 255, 0.18);
    background: rgba(0, 0, 0, 0.28);
    color: inherit;
  }
  .result {
    margin-top: 8px;
    font-size: 12px;
  }
  .result.good {
    color: #6fe08a;
  }
  .result.bad-text {
    color: #ff8a80;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12.5px;
  }
  .hint.error {
    color: #ff9a8f;
  }
  .hint {
    font-size: 11.5px;
    color: rgba(255, 255, 255, 0.45);
    line-height: 1.45;
    margin: 8px 0 0 0;
  }
  .kv {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    font-size: 11.5px;
    padding: 3px 0;
  }
  .kv span {
    color: rgba(255, 255, 255, 0.45);
    flex: none;
  }
  .kv code {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 10.5px;
    color: rgba(255, 255, 255, 0.7);
    text-align: right;
    word-break: break-all;
    user-select: text;
  }
</style>
