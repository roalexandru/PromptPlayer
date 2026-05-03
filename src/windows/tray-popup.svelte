<script lang="ts">
  import { onMount, onDestroy, tick } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
  import { ipc, type Prompt } from "$lib/ipc";
  import { IS_MAC } from "$lib/platform";

  let armed = $state(false);
  let prompts = $state<Prompt[]>([]);
  // hookAlive == false on macOS means the CGEventTap couldn't install (almost
  // always Accessibility permission). The tray surfaces a red banner row that
  // deep-links to System Settings. The Rust-side watcher respawns the hook
  // when permission flips on, so the banner disappears automatically — we
  // re-poll on every popover show to pick that up without a heavier event bus.
  let hookAlive = $state(true);
  let unlisten: UnlistenFn | null = null;
  let unlistenUpdate: UnlistenFn | null = null;
  let rootEl = $state<HTMLDivElement | null>(null);

  // Updater state. `update` is null when no update is known to be available.
  // `checkState` flips between idle/checking/checked/error to drive the menu
  // entry label without leaking error detail to the user.
  type CheckState =
    | { kind: "idle" }
    | { kind: "checking" }
    | { kind: "up-to-date" }
    | { kind: "error" }
    | { kind: "available"; version: string }
    | { kind: "installing" };
  let checkState = $state<CheckState>({ kind: "idle" });
  let currentVersion = $state<string>("");
  // JS-driven hover. CSS :hover is unreliable inside non-activating NSPanels
  // because WKWebView doesn't always dispatch mouse-moved events to the
  // CSS engine. Tracking hover in Svelte state bypasses the issue entirely.
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
      hookAlive = await ipc.isHookAlive();
    } catch {
      // If the IPC fails (shouldn't happen) assume alive — better to under-
      // report than spam the user with a false-positive permission banner.
      hookAlive = true;
    }
  }

  async function fixAccessibility() {
    try {
      await ipc.openAccessibilitySettings();
    } catch (e) {
      console.error("open accessibility failed", e);
    }
    await dismiss();
  }

  async function toggleArmed() {
    armed = await ipc.toggleArmed();
  }

  // Left-click on a pinned prompt FIRES it (Apple Shortcuts behavior). The
  // tray popup hides itself; the menu bar / system tray click never activated
  // the app, so the user's foreground app is still focused — the typing
  // engine delivers into it.
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
      const playing = await ipc.isPlaying();
      if (playing) {
        const ok = window.confirm(
          "A prompt is currently typing. Quit anyway? The remaining text will not be delivered.",
        );
        if (!ok) return;
      }
    } catch {}
    await ipc.trayQuit();
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

  // The tray menu only surfaces the updater row when there's actually something
  // to act on (an available update, or an in-progress install). Manual "Check
  // for Updates" lives in the About window; the 6h background poller flips
  // checkState to `available` when a new release publishes, and that's when
  // the row appears.
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

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      if (ctxMenu) ctxMenu = null;
      else dismiss();
    }
  }

  // Compute the hover key from a pointer position by hit-testing the DOM.
  // We don't rely on per-row mouseenter/leave because WKWebView inside a
  // non-activating NSPanel does not reliably dispatch those events, leaving
  // the CSS :hover state frozen wherever the last received mouse event was.
  function updateHoverFromEvent(e: MouseEvent | PointerEvent) {
    const el = document.elementFromPoint(e.clientX, e.clientY);
    const row = (el as HTMLElement | null)?.closest<HTMLElement>("[data-hkey]");
    hoverKey = row?.dataset.hkey ?? null;
  }
  function clearHover() { hoverKey = null; }

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

  // Some WKWebView + NSPanel combos drop mouse-moved events. As a last
  // resort, poll on rAF using the most recent known cursor position.
  // Cheap (no allocation; one elementFromPoint per frame).
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

  // Block the WebView's default browser context menu (Reload / Inspect
  // Element) on rows other than prompts. Prompt rows have their own custom
  // context menu via `onPromptContextMenu`.
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

  function dismissCtxOnOutsideClick() {
    if (ctxMenu) ctxMenu = null;
  }

  onMount(async () => {
    await refresh();
    currentVersion = await ipc.updaterCurrentVersion();
    // Background poller in Rust emits `update-available` when its 6h check
    // turns up a new release. Surface it in the menu without forcing the
    // user to click "Check for updates…".
    unlistenUpdate = await listen<{ version: string; notes: string | null }>(
      "update-available",
      (e) => { checkState = { kind: "available", version: e.payload.version }; },
    );
    await fitWindow();
    document.addEventListener("mousemove", onAnyMouseMove, true);
    document.addEventListener("pointermove", onAnyMouseMove, true);
    document.addEventListener("mouseover", onAnyMouseMove, true);
    document.addEventListener("mouseleave", onLeaveDocument, true);
    document.addEventListener("mouseout", (e) => {
      if (!e.relatedTarget) onLeaveDocument();
    }, true);
    document.addEventListener("contextmenu", blockBrowserContextMenu);
    startPoll();
    unlisten = await listen("tray-popup-show", async () => {
      await refresh();
      ctxMenu = null;
      await fitWindow();
    });
    // Native bridge: macOS NSEvent monitor in Rust feeds us cursor positions
    // (x, y in CSS px relative to the popover window) since WKWebView in a
    // non-activating NSPanel drops mouse-moved events. WebView2 on Windows
    // dispatches mouse-move natively, so the bridge is never installed and
    // we skip subscribing to avoid a no-op IPC listener.
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
    document.removeEventListener("contextmenu", blockBrowserContextMenu);
  });

  $effect(() => {
    void prompts;
    void armed;
    void ctxMenu;
    fitWindow();
  });

  // §2 — tray shows ONLY pinned prompts (Apple Shortcuts model). Unpinned
  // prompts still fire from triggers; they're just not in the menu.
  const pinnedPrompts = $derived(prompts.filter((p) => p.pinned));
  const hasAnyPrompts = $derived(prompts.length > 0);
  // Source of truth: src-tauri/src/app/shortcuts.rs.
  // Picker (Command Palette): Modifiers::ALT | PRIMARY + Code::Backslash
  //   → Mac: ⌘⌥\, Win: Ctrl+Alt+\.
  // Library has no global shortcut registered — don't show one in the menu.
  const primaryShortcut = IS_MAC ? "⌘⌥\\" : "Ctrl+Alt+\\";
  const quitShortcut = IS_MAC ? "⌘Q" : "Ctrl+Q";
</script>

<svelte:window on:keydown={onKey} />

<div
  class="root"
  bind:this={rootEl}
  onmousemove={updateHoverFromEvent}
  onmouseover={updateHoverFromEvent}
  onmouseleave={clearHover}
  onclick={dismissCtxOnOutsideClick}
>
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

  {#if !hookAlive && IS_MAC}
    <!-- Surface the silent-fail mode where Accessibility wasn't granted (or
         was revoked). Without this banner the user sees toggle=on, types a
         trigger, and nothing happens — the worst kind of silent failure. -->
    <button
      class="row warn"
      class:hover={hoverKey === "ax"}
      data-hkey="ax"
      onclick={fixAccessibility}
      title="The keyboard listener is not running"
    >
      <span class="warn-dot"></span>
      <span class="label">Grant Accessibility…</span>
    </button>
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
  {/if}
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
    .row.hover:not(.disabled) {
      background: rgba(0, 0, 0, 0.10);
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
</style>
