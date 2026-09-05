<script lang="ts">
  import { onMount, onDestroy, tick } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
  import { confirm } from "@tauri-apps/plugin-dialog";
  import { ipc, type Prompt, type KeepAwakeState } from "$lib/ipc";
  import { IS_MAC } from "$lib/platform";

  let armed = $state(false);
  let prompts = $state<Prompt[]>([]);
  // True while a typing playback is in flight — drives the emergency
  // "Stop typing" row at the top of the popup.
  let playing = $state(false);
  // Hook health. Tri-state on purpose: "unknown" is not "fine", and defaulting
  // a failed read to alive hides the one warning that matters.
  let hookStatus = $state<"ok" | "dead" | "unknown">("ok");
  // macOS Secure Input is engaged, so triggers are gated off right now.
  let secureInput = $state(false);
  // Keep-awake, with the auto-off the backend is enforcing.
  let keepAwake = $state<KeepAwakeState | null>(null);
  let showAwakeMenu = $state(false);
  // Transport state: pausing mid-prompt is the gesture the kill switch can't
  // serve, since killing throws the rest of the body away.
  let paused = $state(false);
  let speed = $state(1);
  // Setlist cues, so "next cue" is reachable without a hotkey.
  let cueCount = $state(0);
  let cueIndex = $state(0);
  let unlisten: UnlistenFn | null = null;
  let unlistenUpdate: UnlistenFn | null = null;
  let rootEl = $state<HTMLDivElement | null>(null);

  // Updater state. `checkState` drives the row's label without surfacing any
  // error detail to the user.
  type CheckState =
    | { kind: "idle" }
    | { kind: "checking" }
    | { kind: "up-to-date" }
    | { kind: "error" }
    | { kind: "available"; version: string }
    | { kind: "installing" };
  let checkState = $state<CheckState>({ kind: "idle" });
  let currentVersion = $state<string>("");
  // JS hover: WKWebView in a non-activating NSPanel doesn't reliably feed
  // mouse-moved events to the CSS engine, so `:hover` misses.
  let hoverKey = $state<string | null>(null);

  // Right-click context menu — anchored at click position; one prompt at a
  // time. Replaces the WebView's default "Reload / Inspect Element" menu.
  type CtxMenu = { x: number; y: number; prompt: Prompt };
  let ctxMenu = $state<CtxMenu | null>(null);

  async function fitWindow() {
    if (!rootEl) return;
    await tick();
    // Read scrollHeight to capture content even when CSS height: auto.
    // setSize accepts logical pixels — match it to body's content height.
    const h = rootEl.offsetHeight;
    if (h > 0) {
      try {
        await getCurrentWindow().setSize(new LogicalSize(280, h));
      } catch {}
    }
  }

  async function refresh() {
    armed = await ipc.getArmed();
    prompts = await ipc.listPrompts();
    try {
      playing = await ipc.isPlaying();
    } catch {
      playing = false;
    }
    try {
      // One read covers hook health and the Secure Input gate, which is
      // otherwise invisible while it silently swallows every trigger.
      const d = await ipc.getDiagnostics();
      hookStatus = d.hookAlive ? "ok" : "dead";
      secureInput = d.secureInputActive;
    } catch {
      hookStatus = "unknown";
      secureInput = false;
    }
    try {
      keepAwake = await ipc.getKeepAwake();
    } catch {
      keepAwake = null;
    }
    try {
      const status = await ipc.playbackStatus();
      playing = status.playing;
      paused = status.paused;
      speed = status.speed;
    } catch {
      paused = false;
      speed = 1;
    }
    try {
      const cues = await ipc.getSetlist();
      cueCount = cues.length;
      cueIndex = Math.max(0, cues.findIndex((c) => c.isNext));
    } catch {
      cueCount = 0;
      cueIndex = 0;
    }
  }

  // Send the user to the diagnostics window rather than straight to System
  // Settings: if the checkbox is already ticked, the pane alone doesn't help.
  async function openDiagnostics() {
    try {
      await ipc.openDiagnostics();
    } catch (e) {
      console.error("open diagnostics failed", e);
    }
    await dismiss();
  }

  async function toggleArmed() {
    armed = await ipc.toggleArmed();
  }

  async function toggleKeepAwake() {
    try {
      keepAwake = await ipc.toggleKeepAwake();
    } catch (e) {
      console.error("toggle keep-awake failed", e);
    }
  }

  async function pickAwakeDuration(mins: number) {
    showAwakeMenu = false;
    try {
      keepAwake = await ipc.setKeepAwakeDuration(mins);
      if (!keepAwake.enabled) keepAwake = await ipc.toggleKeepAwake(mins);
    } catch (e) {
      console.error("set keep-awake duration failed", e);
    }
  }

  function durationLabel(mins: number): string {
    if (mins === 0) return "Until I turn it off";
    return mins >= 60 ? `${mins / 60} hour${mins === 60 ? "" : "s"}` : `${mins} minutes`;
  }

  // "Keep Awake · 1h 12m left" — an eight-hour session shouldn't hide behind
  // an anonymous checkmark.
  const keepAwakeSuffix = $derived.by(() => {
    if (!keepAwake?.enabled) return "";
    const secs = keepAwake.remainingSecs;
    if (secs == null) return "no limit";
    const mins = Math.ceil(secs / 60);
    return mins >= 60 ? `${Math.floor(mins / 60)}h ${mins % 60}m left` : `${mins}m left`;
  });

  // Left-click fires it (Apple Shortcuts behavior). The tray click never
  // activated us, so the user's app still has focus to receive the typing.
  async function firePrompt(p: Prompt) {
    try {
      await ipc.trayFirePrompt(p.id);
    } catch (e) {
      // Surfacing this in the tray UI is awkward — log and stay silent. The
      // backend already telemetered the failure.
      console.error("tray fire failed", e);
    }
  }

  async function dismiss() {
    await ipc.trayPopupHide();
  }

  async function action(label: "library" | "picker" | "about") {
    await ipc.trayOpen(label);
    await dismiss();
  }

  // Quit, but if a fire is in flight, ask first — interrupting mid-stream
  // leaves the user's target app with a half-typed prompt.
  async function quit() {
    try {
      const inFlight = await ipc.isPlaying();
      if (inFlight) {
        const ok = await confirm(
          "A prompt is currently typing. Quit anyway? The remaining text will not be delivered.",
        );
        if (!ok) return;
      }
    } catch {}
    await ipc.trayQuit();
  }

  // Emergency stop — abort the in-flight playback, then re-read state so the
  // row disappears once the engine reports idle.
  async function stopTyping() {
    try {
      await ipc.kill();
    } catch (e) {
      console.error("kill failed", e);
    }
    await refresh();
  }

  // §3.5 — freeze the run so you can narrate, then pick it up where it
  // stopped. Unlike Stop, nothing is lost.
  async function togglePause() {
    try {
      const next = await ipc.togglePlaybackPause();
      if (next !== null) paused = next;
    } catch (e) {
      console.error("pause failed", e);
    }
    await refresh();
  }

  async function nudgeSpeed(faster: boolean) {
    try {
      const next = await ipc.nudgePlaybackSpeed(faster);
      if (next !== null) speed = next;
    } catch (e) {
      console.error("speed nudge failed", e);
    }
  }

  // Fire the next cue, then dismiss so the typing lands in the user's app.
  async function nextCue() {
    try {
      await ipc.fireNextCue();
    } catch (e) {
      console.error("next cue failed", e);
    }
    await dismiss();
  }

  async function rewindSetlist() {
    try {
      await ipc.resetSetlist();
    } catch (e) {
      console.error("rewind failed", e);
    }
    await refresh();
  }

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
      // Resolves only after the host process restarts, so the UI never sees
      // a "completed" state — the new app starts fresh.
      await ipc.updaterInstall();
    } catch {
      checkState = { kind: "error" };
    }
  }

  // Only shown when there's something to act on. Manual checks live in About;
  // the 6h poller flips `checkState` when a release lands.
  const showUpdateRow = $derived(
    checkState.kind === "available" || checkState.kind === "installing"
  );
  const updateLabel = $derived.by(() => {
    switch (checkState.kind) {
      case "available":  return `Install Update v${checkState.version}`;
      case "installing": return "Installing Update — Restarting…";
      default:           return "";
    }
  });
  const updateClickable = $derived(checkState.kind === "available");
  function onUpdateClick() {
    if (checkState.kind === "available") installUpdate();
  }

  // Clears the tray badge and stops the nag until a newer version ships.
  async function skipUpdate() {
    if (checkState.kind !== "available") return;
    const version = checkState.version;
    checkState = { kind: "idle" };
    try {
      await ipc.updaterDismiss(version);
    } catch (e) {
      console.error("dismiss update failed", e);
    }
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      if (ctxMenu) ctxMenu = null;
      else dismiss();
    }
  }

  let lastClientX = 0;
  let lastClientY = 0;
  let pollHandle: number | undefined;

  function pickKeyAt(x: number, y: number): string | null {
    const el = document.elementFromPoint(x, y);
    const row = (el as HTMLElement | null)?.closest<HTMLElement>("[data-hkey]");
    return row?.dataset.hkey ?? null;
  }

  function onAnyMouseMove(e: MouseEvent | PointerEvent) {
    lastClientX = e.clientX;
    lastClientY = e.clientY;
    hoverKey = pickKeyAt(e.clientX, e.clientY);
  }
  function onLeaveDocument() { hoverKey = null; }
  function onAnyMouseOut(e: MouseEvent) {
    if (!e.relatedTarget) onLeaveDocument();
  }

  // Last resort for WKWebView+NSPanel combos that drop mouse-moved entirely:
  // one `elementFromPoint` per frame against the last known cursor position.
  function startPoll() {
    let prev: string | null = null;
    const tick = () => {
      const k = pickKeyAt(lastClientX, lastClientY);
      if (k !== prev) {
        prev = k;
        hoverKey = k;
      }
      pollHandle = requestAnimationFrame(tick);
    };
    pollHandle = requestAnimationFrame(tick);
  }

  let unlistenMouseMove: UnlistenFn | null = null;

  // Suppress the browser context menu everywhere except prompt rows, which
  // have their own via `onPromptContextMenu`.
  function blockBrowserContextMenu(e: MouseEvent) {
    e.preventDefault();
  }

  function onPromptContextMenu(e: MouseEvent, p: Prompt) {
    e.preventDefault();
    e.stopPropagation();
    // Position relative to the popup body. Clamp to keep within bounds —
    // the menu is ~200px wide and ~120px tall.
    const padX = 6;
    const padY = 6;
    const menuW = 200;
    const menuH = 130;
    const winW = window.innerWidth;
    const winH = window.innerHeight;
    let x = e.clientX;
    let y = e.clientY;
    if (x + menuW + padX > winW) x = winW - menuW - padX;
    if (y + menuH + padY > winH) y = winH - menuH - padY;
    if (x < padX) x = padX;
    if (y < padY) y = padY;
    ctxMenu = { x, y, prompt: p };
  }

  async function ctxRun() {
    if (!ctxMenu) return;
    const p = ctxMenu.prompt;
    ctxMenu = null;
    await firePrompt(p);
  }

  async function ctxEdit() {
    if (!ctxMenu) return;
    ctxMenu = null;
    await action("library");
    // TODO: deep-link to the specific prompt — needs a `library:focus` event
    // round-trip. v1.1.
  }

  async function ctxToggleEnabled() {
    if (!ctxMenu) return;
    const p = ctxMenu.prompt;
    ctxMenu = null;
    const next = !p.enabled;
    prompts = prompts.map((x) => (x.id === p.id ? { ...x, enabled: next } : x));
    try {
      await ipc.setPromptEnabled(p.id, next);
    } catch {
      prompts = prompts.map((x) => (x.id === p.id ? { ...x, enabled: !next } : x));
    }
  }

  async function ctxUnpin() {
    if (!ctxMenu) return;
    const p = ctxMenu.prompt;
    ctxMenu = null;
    // Optimistic: drop from the visible list immediately.
    prompts = prompts.map((x) => (x.id === p.id ? { ...x, pinned: false } : x));
    try {
      await ipc.setPromptPinned(p.id, false);
    } catch {
      prompts = prompts.map((x) => (x.id === p.id ? { ...x, pinned: true } : x));
    }
  }

  function dismissCtxOnOutsidePointer(e: PointerEvent) {
    if (!ctxMenu) return;
    const target = e.target as HTMLElement | null;
    if (target?.closest(".ctx")) return;
    ctxMenu = null;
  }

  onMount(async () => {
    await refresh();
    currentVersion = await ipc.updaterCurrentVersion();
    // The Rust poller emits this on its 6h check, so the row appears without
    // the user having to go looking.
    unlistenUpdate = await listen<{ version: string; notes: string | null }>(
      "update-available",
      (e) => {
        checkState = { kind: "available", version: e.payload.version };
        // Records that the affordance was actually rendered — the update
        // funnel used to be a single point with no shown/dismissed steps.
        ipc.updaterAnnounced(e.payload.version).catch(() => {});
      },
    );
    await fitWindow();
    document.addEventListener("mousemove", onAnyMouseMove, true);
    document.addEventListener("pointermove", onAnyMouseMove, true);
    document.addEventListener("mouseover", onAnyMouseMove, true);
    document.addEventListener("mouseleave", onLeaveDocument, true);
    document.addEventListener("mouseout", onAnyMouseOut, true);
    document.addEventListener("pointerdown", dismissCtxOnOutsidePointer, true);
    document.addEventListener("contextmenu", blockBrowserContextMenu);
    startPoll();
    unlisten = await listen("tray-popup-show", async () => {
      await refresh();
      ctxMenu = null;
      await fitWindow();
    });
    // macOS only: an NSEvent monitor in Rust feeds cursor positions, since the
    // panel's webview drops mouse-moved. WebView2 dispatches them natively.
    if (IS_MAC) {
      unlistenMouseMove = await listen<[number, number]>(
        "tray-popup-mousemove",
        (e) => {
          const [x, y] = e.payload;
          lastClientX = x;
          lastClientY = y;
          if (x < 0 || y < 0) {
            hoverKey = null;
            return;
          }
          hoverKey = pickKeyAt(x, y);
        },
      );
    }
  });

  onDestroy(() => {
    unlisten?.();
    unlistenUpdate?.();
    unlistenMouseMove?.();
    if (pollHandle !== undefined) cancelAnimationFrame(pollHandle);
    document.removeEventListener("mousemove", onAnyMouseMove, true);
    document.removeEventListener("pointermove", onAnyMouseMove, true);
    document.removeEventListener("mouseover", onAnyMouseMove, true);
    document.removeEventListener("mouseleave", onLeaveDocument, true);
    document.removeEventListener("mouseout", onAnyMouseOut, true);
    document.removeEventListener("pointerdown", dismissCtxOnOutsidePointer, true);
    document.removeEventListener("contextmenu", blockBrowserContextMenu);
  });

  $effect(() => {
    void prompts;
    void armed;
    void ctxMenu;
    void playing;
    fitWindow();
  });

  // §2 — tray shows ONLY pinned prompts (Apple Shortcuts model). Unpinned
  // prompts still fire from triggers; they're just not in the menu.
  const pinnedPrompts = $derived(prompts.filter((p) => p.pinned));
  const hasAnyPrompts = $derived(prompts.length > 0);
  // Mirrors src-tauri/src/app/shortcuts.rs — ⌘⌥\ on Mac, Ctrl+Alt+\ on Windows.
  // Library has no global shortcut, so it shows none.
  const primaryShortcut = IS_MAC ? "⌘⌥\\" : "Ctrl+Alt+\\";
  const quitShortcut = IS_MAC ? "⌘Q" : "Ctrl+Q";
</script>

<svelte:window on:keydown={onKey} />

<div
  class="root"
  class:opaque={!IS_MAC}
  bind:this={rootEl}
>
  {#if playing}
    <!-- Emergency stop — a playback is typing into the foreground app right
         now. Most prominent row in the popup; aborts via the same kill
         pipeline as the global kill-switch. -->
    <button
      class="row stop"
      class:hover={hoverKey === "stop"}
      data-hkey="stop"
      onclick={stopTyping}
      title="Abort the in-flight typing playback"
    >
      <span class="stop-dot"></span>
      <span class="label">Stop typing</span>
    </button>
    <button
      class="row plain"
      class:hover={hoverKey === "pause"}
      data-hkey="pause"
      onclick={togglePause}
      title="Freeze the playback without losing the rest of the prompt"
    >
      <span class="label">{paused ? "Resume typing" : "Pause typing"}</span>
      <span class="hint">{IS_MAC ? "⌘⇧," : "Ctrl+Shift+,"}</span>
    </button>
    <div class="speed-row">
      <button class="speed-btn" onclick={() => nudgeSpeed(false)} title="Slower">−</button>
      <span class="speed-val">×{speed.toFixed(2)}</span>
      <button class="speed-btn" onclick={() => nudgeSpeed(true)} title="Faster">+</button>
    </div>
    <div class="sep"></div>
  {/if}

  {#if cueCount > 0}
    <!-- Setlist transport. One row, always the same target, so recall never
         enters into it mid-demo. -->
    <button
      class="row plain"
      class:hover={hoverKey === "cue"}
      data-hkey="cue"
      onclick={nextCue}
      title="Fire the next cue in the setlist"
    >
      <span class="label">Next cue ({cueIndex + 1} of {cueCount})</span>
      <span class="hint">{IS_MAC ? "⌘⇧." : "Ctrl+Shift+."}</span>
    </button>
    <button
      class="row plain"
      class:hover={hoverKey === "rewind"}
      data-hkey="rewind"
      onclick={rewindSetlist}
      title="Back to the first cue"
    >
      <span class="label">Rewind setlist</span>
    </button>
    <div class="sep"></div>
  {/if}

  <!-- Header — title + native toggle switch. The toggle alone carries the
       state; no verb in the title (#1 option C). Aria-label preserves the
       semantic meaning for screen readers. -->
  <div class="header">
    <span class="title">Prompt Player</span>
    <button
      class="switch"
      class:on={armed}
      role="switch"
      aria-checked={armed}
      aria-label={armed ? "Active — typing triggers will fire" : "Inactive — typing triggers won't fire"}
      onclick={toggleArmed}
    >
      <span class="knob"></span>
    </button>
  </div>

  <div class="sep"></div>

  {#if hookStatus !== "ok"}
    <!-- Without this the user sees toggle=on, types a trigger, and nothing
         happens. "unknown" shows too — a failed read is not reassurance. -->
    <button
      class="row warn"
      class:hover={hoverKey === "ax"}
      data-hkey="ax"
      onclick={openDiagnostics}
      title="The keyboard listener is not running"
    >
      <span class="warn-dot"></span>
      <span class="label">
        {hookStatus === "dead" ? "Triggers won't fire — fix…" : "Status unavailable — check…"}
      </span>
    </button>
    <div class="sep"></div>
  {:else if secureInput}
    <!-- The gate is shut right now: keystrokes pass straight through and no
         trigger can match. Silent until this row existed. -->
    <div class="row warn static" title="macOS Secure Input is active">
      <span class="warn-dot"></span>
      <span class="label">Secure Input active — triggers paused</span>
    </div>
    <div class="sep"></div>
  {/if}

  <!-- Pinned prompts. Empty state surfaces the Quick Start hint (#9). -->
  {#if pinnedPrompts.length === 0}
    <div class="empty">
      {#if hasAnyPrompts}
        <div class="empty-title">Pin prompts to see them here</div>
        <div class="empty-hint">Open the library, hover a prompt, click the pin icon.</div>
      {:else}
        <div class="empty-title">No prompts yet</div>
        <div class="empty-hint">
          Type a trigger like <span class="kbd">hello&gt;</span> in any text field to fire a prompt.
          Open the library to add your first one.
        </div>
      {/if}
    </div>
  {:else}
    {#each pinnedPrompts as p (p.id)}
      <button
        class="row prompt"
        class:hover={hoverKey === `p:${p.id}`}
        class:dim={!armed || !p.enabled}
        data-hkey={`p:${p.id}`}
        onclick={() => firePrompt(p)}
        oncontextmenu={(e) => onPromptContextMenu(e, p)}
        title={p.name}
      >
        <span class="label">{p.name}</span>
        {#if p.triggers.length > 0}
          <span class="trigger" title="Trigger word">{p.triggers[0]}&gt;</span>
        {/if}
      </button>
    {/each}
  {/if}

  <div class="sep"></div>

  <button
    class="row plain"
    class:hover={hoverKey === "library"}
    data-hkey="library"
    onclick={() => action("library")}
  >
    <span class="label">Prompt Library</span>
  </button>
  <button
    class="row plain"
    class:hover={hoverKey === "picker"}
    data-hkey="picker"
    onclick={() => action("picker")}
  >
    <span class="label">Command Palette…</span>
    <span class="shortcut">{primaryShortcut}</span>
  </button>
  <!-- Keep Awake — checkmark matches the Windows menu's MF_CHECKED; the
       suffix shows the auto-off so a long session can't go unnoticed. -->
  <button
    class="row plain"
    class:hover={hoverKey === "keepawake"}
    data-hkey="keepawake"
    role="menuitemcheckbox"
    aria-checked={keepAwake?.enabled ?? false}
    onclick={toggleKeepAwake}
    title="Prevent the screen from sleeping or the screensaver from starting"
  >
    <span class="label">Keep Awake</span>
    {#if keepAwakeSuffix}<span class="shortcut">{keepAwakeSuffix}</span>{/if}
    {#if keepAwake?.enabled}<span class="check">✓</span>{/if}
  </button>
  <button
    class="row plain sub"
    class:hover={hoverKey === "awakedur"}
    data-hkey="awakedur"
    onclick={() => (showAwakeMenu = !showAwakeMenu)}
  >
    <span class="label">Keep Awake for…</span>
    <span class="shortcut">{durationLabel(keepAwake?.defaultMins ?? 120)}</span>
  </button>
  {#if showAwakeMenu && keepAwake}
    {#each keepAwake.choices as mins}
      <button
        class="row plain sub indent"
        class:hover={hoverKey === `awake-${mins}`}
        data-hkey={`awake-${mins}`}
        onclick={() => pickAwakeDuration(mins)}
      >
        <span class="label">{durationLabel(mins)}</span>
        {#if keepAwake.defaultMins === mins}<span class="check">✓</span>{/if}
      </button>
    {/each}
  {/if}

  <div class="sep"></div>

  {#if showUpdateRow}
    <button
      class="row plain"
      class:hover={hoverKey === "update" && updateClickable}
      class:disabled={!updateClickable}
      class:accent={checkState.kind === "available"}
      data-hkey="update"
      onclick={onUpdateClick}
      disabled={!updateClickable}
    >
      <span class="label">{updateLabel}</span>
    </button>
    {#if checkState.kind === "available"}
      <button
        class="row plain sub"
        class:hover={hoverKey === "update-skip"}
        data-hkey="update-skip"
        onclick={skipUpdate}
      >
        <span class="label">Skip this version</span>
      </button>
    {/if}
  {/if}
  <button
    class="row plain"
    class:hover={hoverKey === "diagnostics"}
    data-hkey="diagnostics"
    onclick={openDiagnostics}
  >
    <span class="label">Diagnostics…</span>
  </button>
  <button
    class="row plain"
    class:hover={hoverKey === "about"}
    data-hkey="about"
    onclick={() => action("about")}
  >
    <span class="label">About Prompt Player</span>
  </button>
  <button
    class="row plain"
    class:hover={hoverKey === "quit"}
    data-hkey="quit"
    onclick={quit}
  >
    <span class="label">Quit</span>
    <span class="shortcut">{quitShortcut}</span>
  </button>
</div>

<!-- Custom right-click context menu. Replaces the WebView's default Reload /
     Inspect Element. Anchored at click coords; dismisses on outside click,
     Esc, or any action. -->
{#if ctxMenu}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <!-- svelte-ignore a11y_interactive_supports_focus -->
  <div
    class="ctx"
    style="left: {ctxMenu.x}px; top: {ctxMenu.y}px;"
    onclick={(e) => e.stopPropagation()}
    role="menu"
    tabindex="-1"
  >
    <button class="ctx-row" onclick={ctxRun}>
      <span class="label">Run</span>
      <span class="shortcut">⏎</span>
    </button>
    <button class="ctx-row" onclick={ctxEdit}>
      <span class="label">Edit in Library</span>
    </button>
    <div class="ctx-sep"></div>
    <button class="ctx-row" onclick={ctxToggleEnabled}>
      <span class="label">{ctxMenu.prompt.enabled ? "Disable" : "Enable"}</span>
    </button>
    <button class="ctx-row" onclick={ctxUnpin}>
      <span class="label">Unpin</span>
    </button>
  </div>
{/if}

<style>
  :global(html), :global(body), :global(#app) {
    margin: 0;
    padding: 0;
    background: transparent !important;
    overflow: hidden;
    font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text",
      "Segoe UI", sans-serif;
    -webkit-font-smoothing: antialiased;
    color: rgba(255, 255, 255, 0.92);
    font-size: 13px;
  }
  :global(html), :global(body) {
    height: auto;
  }
  :global(#app) {
    height: auto;
  }

  /* The window itself is the rounded popover (NSVisualEffectMaterial::Menu
     with corner radius 6). The body sits flush — no inner card. */
  .root {
    box-sizing: border-box;
    padding: 4px 0;
  }
  /* Windows fallback — see picker.svelte for the full rationale. WebView2
     on Win11 24H2 can render the transparent body as solid white, making
     white-on-transparent text invisible. Paint our own dark surface so
     the popup is always readable; Mica still composites when it works. */
  .root.opaque {
    background: rgba(28, 28, 30, 0.97);
    border-radius: 8px;
  }

  /* Compact header — single row, native scale (~28px). */
  .header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 3px 12px 3px 12px;
    height: 26px;
  }
  .title {
    font-size: 13px;
    font-weight: 600;
    letter-spacing: -0.05px;
  }

  /* Apple-style toggle switch, scaled to 28×16 for menu density. */
  .switch {
    position: relative;
    width: 28px;
    height: 16px;
    border-radius: 999px;
    background: rgba(120, 120, 128, 0.32);
    border: none;
    padding: 0;
    cursor: default;
    transition: background-color 120ms ease;
    flex-shrink: 0;
  }
  .switch.on {
    background: rgba(48, 209, 88, 1);
  }
  .switch .knob {
    position: absolute;
    top: 1px;
    left: 1px;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 0.5px 1px rgba(0, 0, 0, 0.2), 0 1px 2px rgba(0, 0, 0, 0.18);
    transition: transform 140ms ease;
  }
  .switch.on .knob {
    transform: translateX(12px);
  }

  .row {
    display: flex;
    align-items: center;
    width: calc(100% - 8px);
    margin: 0 4px;
    padding: 3px 8px 3px 12px;
    background: transparent;
    border: none;
    text-align: left;
    color: inherit;
    font: inherit;
    font-size: 13px;
    line-height: 1.3;
    border-radius: 4px;
    cursor: pointer;
    user-select: none;
    -webkit-user-select: none;
    height: 22px;
    transition: background-color 60ms ease, opacity 80ms ease;
    pointer-events: auto;
  }

  /* Hover is driven exclusively by JS state via mousemove hit-testing on the
     root, then `class:hover` on each row. CSS `:hover` is intentionally NOT
     used because WKWebView inside a non-activating NSPanel does not
     reliably dispatch mouse-enter / mouse-leave, leaving :hover stuck. */
  .row.hover:not(.disabled) {
    background: rgba(255, 255, 255, 0.18);
    color: #fff;
  }

  .row.disabled {
    opacity: 0.5;
    cursor: default;
  }

  /* #7 — when toggle is off (or prompt is disabled), dim the prompt rows.
     Hover still works (still discoverable as clickable) but the visual
     weight tells you nothing fires right now. */
  .row.dim {
    opacity: 0.45;
  }
  .row.dim:hover, .row.dim.hover {
    opacity: 0.65;
  }

  /* Accent state for "Install update" — uses the same green as the armed
     toggle so the badge color is consistent across the popup. */
  .row.accent .label {
    color: rgba(48, 209, 88, 1);
    font-weight: 600;
  }

  /* Emergency "Stop typing" row — red, bold, sits above everything else
     while a playback is in flight. */
  .row.stop .label {
    color: rgba(255, 69, 58, 1);
    font-weight: 600;
  }
  .stop-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: rgba(255, 69, 58, 1);
    margin-right: 8px;
    flex-shrink: 0;
    box-shadow: 0 0 6px rgba(255, 69, 58, 0.6);
  }
  .row.stop.hover {
    background: rgba(255, 69, 58, 0.18);
  }

  /* Warning state for the "Grant Accessibility" banner. macOS-y orange. */
  .row.warn .label {
    color: rgba(255, 159, 10, 1);
    font-weight: 600;
  }
  .warn-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: rgba(255, 159, 10, 1);
    margin-right: 8px;
    flex-shrink: 0;
    box-shadow: 0 0 6px rgba(255, 159, 10, 0.6);
  }
  /* Status text, not an action — no hover affordance. */
  .row.static {
    cursor: default;
  }
  /* Secondary line under the row it belongs to. */
  .row.sub .label {
    padding-left: 14px;
    color: rgba(255, 255, 255, 0.6);
  }
  .row.sub.indent .label {
    padding-left: 26px;
  }

  .label {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    margin-right: 8px;
  }

  /* #3 — trigger word right-aligned, dimmed monospace, like a keyboard
     shortcut hint. Teaches the user the trigger by repeated exposure. */
  .trigger {
    font-family: "SF Mono", Menlo, Consolas, monospace;
    font-size: 11px;
    color: rgba(255, 255, 255, 0.45);
    flex-shrink: 0;
    letter-spacing: -0.2px;
  }
  .row.prompt.hover .trigger {
    color: rgba(255, 255, 255, 0.7);
  }

  /* #4 — keyboard shortcut hints on system items (Library, Palette, Quit). */
  .shortcut {
    font-size: 11px;
    color: rgba(255, 255, 255, 0.45);
    flex-shrink: 0;
    margin-left: 8px;
    letter-spacing: 0.2px;
  }
  .row.plain.hover .shortcut {
    color: rgba(255, 255, 255, 0.7);
  }

  /* Checkmark for the Keep Awake toggle — right-aligned, brighter than the
     shortcut hint so the "on" state reads at a glance. */
  .check {
    font-size: 12px;
    color: rgba(48, 209, 88, 1);
    flex-shrink: 0;
    margin-left: 8px;
    font-weight: 700;
  }

  .sep {
    height: 1px;
    background: rgba(255, 255, 255, 0.13);
    margin: 4px 12px;
  }

  /* #9 — empty-state Quick Start panel. Replaces the bare "No prompts" row
     with a two-line hint that teaches what the tray is for. */
  .empty {
    padding: 8px 14px 10px 14px;
    color: rgba(255, 255, 255, 0.55);
  }
  .empty-title {
    font-size: 12px;
    font-weight: 600;
    margin-bottom: 3px;
    color: rgba(255, 255, 255, 0.75);
  }
  .empty-hint {
    font-size: 11.5px;
    line-height: 1.4;
  }
  .kbd {
    font-family: "SF Mono", Menlo, Consolas, monospace;
    font-size: 11px;
    background: rgba(255, 255, 255, 0.12);
    border-radius: 3px;
    padding: 1px 5px;
    color: rgba(255, 255, 255, 0.85);
  }

  /* #8 — custom right-click context menu. Floats over the popup, anchored
     at click coords. Visually consistent with the popup itself (same blur,
     border, hover treatment). */
  .ctx {
    position: fixed;
    min-width: 180px;
    padding: 4px 0;
    background: rgba(40, 40, 42, 0.97);
    border: 1px solid rgba(255, 255, 255, 0.12);
    border-radius: 6px;
    box-shadow: 0 4px 16px rgba(0, 0, 0, 0.4);
    backdrop-filter: blur(20px);
    -webkit-backdrop-filter: blur(20px);
    z-index: 100;
  }
  .ctx-row {
    display: flex;
    align-items: center;
    width: calc(100% - 8px);
    margin: 0 4px;
    padding: 4px 8px 4px 12px;
    background: transparent;
    border: none;
    text-align: left;
    color: inherit;
    font: inherit;
    font-size: 13px;
    border-radius: 4px;
    cursor: default;
    user-select: none;
    -webkit-user-select: none;
    height: 22px;
  }
  .ctx-row:hover {
    background: rgba(10, 132, 255, 0.85);
    color: #fff;
  }
  .ctx-sep {
    height: 1px;
    background: rgba(255, 255, 255, 0.12);
    margin: 4px 8px;
  }

  @media (prefers-color-scheme: light) {
    :global(html), :global(body), :global(#app) {
      color: rgba(0, 0, 0, 0.88);
    }
    /* Windows opaque fallback must flip with the text: the base rule paints
       a near-black surface, but this media block flips text to near-black —
       give the opaque root a light surface (plus a hairline border, since a
       near-white panel otherwise has no edge against light desktops).
       macOS stays translucent (.opaque is only applied when !IS_MAC). */
    .root.opaque {
      background: rgba(250, 250, 250, 0.98);
      border: 1px solid rgba(0, 0, 0, 0.12);
    }
    .row.hover:not(.disabled) {
      background: rgba(0, 0, 0, 0.10);
    }
    .row.stop .label {
      color: rgba(255, 59, 48, 1);
    }
    .stop-dot {
      background: rgba(255, 59, 48, 1);
      box-shadow: 0 0 6px rgba(255, 59, 48, 0.5);
    }
    .row.stop.hover {
      background: rgba(255, 59, 48, 0.12);
    }
    .trigger, .shortcut {
      color: rgba(0, 0, 0, 0.45);
    }
    .row.prompt.hover .trigger, .row.plain.hover .shortcut {
      color: rgba(0, 0, 0, 0.7);
    }
    .empty {
      color: rgba(0, 0, 0, 0.55);
    }
    .empty-title {
      color: rgba(0, 0, 0, 0.78);
    }
    .kbd {
      background: rgba(0, 0, 0, 0.08);
      color: rgba(0, 0, 0, 0.85);
    }
    .sep {
      background: rgba(0, 0, 0, 0.1);
    }
    .ctx {
      background: rgba(252, 252, 252, 0.98);
      border-color: rgba(0, 0, 0, 0.12);
    }
    .ctx-sep {
      background: rgba(0, 0, 0, 0.1);
    }
  }

  /* Speed nudge, shown only while a playback is in flight. */
  .speed-row {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 10px;
    padding: 2px 12px 6px;
  }
  .speed-btn {
    border: 1px solid rgba(255, 255, 255, 0.22);
    background: transparent;
    color: inherit;
    border-radius: 5px;
    width: 22px;
    height: 20px;
    line-height: 1;
    cursor: pointer;
    font-size: 13px;
  }
  .speed-val {
    font-variant-numeric: tabular-nums;
    font-size: 11px;
    opacity: 0.75;
    min-width: 44px;
    text-align: center;
  }
</style>
