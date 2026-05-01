<script lang="ts">
  import { onMount, onDestroy, tick } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
  import { ipc, type Prompt } from "$lib/ipc";
  import { IS_MAC } from "$lib/platform";

  let armed = $state(false);
  let prompts = $state<Prompt[]>([]);
  let unlisten: UnlistenFn | null = null;
  let rootEl = $state<HTMLDivElement | null>(null);
  // JS-driven hover. CSS :hover is unreliable inside non-activating NSPanels
  // because WKWebView doesn't always dispatch mouse-moved events to the
  // CSS engine. Tracking hover in Svelte state bypasses the issue entirely.
  let hoverKey = $state<string | null>(null);

  async function fitWindow() {
    if (!rootEl) return;
    await tick();
    // Read scrollHeight to capture content even when CSS height: auto.
    // setSize accepts logical pixels — match it to body's content height.
    const h = rootEl.offsetHeight;
    if (h > 0) {
      try {
        await getCurrentWindow().setSize(new LogicalSize(260, h));
      } catch {}
    }
  }

  async function refresh() {
    armed = await ipc.getArmed();
    prompts = await ipc.listPrompts();
  }

  async function toggleArmed() {
    armed = await ipc.toggleArmed();
  }

  async function onPromptClick(p: Prompt) {
    const next = !p.enabled;
    prompts = prompts.map((x) => (x.id === p.id ? { ...x, enabled: next } : x));
    try {
      await ipc.setPromptEnabled(p.id, next);
    } catch {
      prompts = prompts.map((x) => (x.id === p.id ? { ...x, enabled: !next } : x));
    }
  }

  async function dismiss() {
    await ipc.trayPopupHide();
  }

  async function action(label: "library" | "picker" | "settings" | "about") {
    await ipc.trayOpen(label);
    await dismiss();
  }

  async function quit() {
    await ipc.trayQuit();
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      dismiss();
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

  onMount(async () => {
    await refresh();
    await fitWindow();
    document.addEventListener("mousemove", onAnyMouseMove, true);
    document.addEventListener("pointermove", onAnyMouseMove, true);
    document.addEventListener("mouseover", onAnyMouseMove, true);
    document.addEventListener("mouseleave", onLeaveDocument, true);
    document.addEventListener("mouseout", (e) => {
      if (!e.relatedTarget) onLeaveDocument();
    }, true);
    startPoll();
    unlisten = await listen("tray-popup-show", async () => {
      await refresh();
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
    unlistenMouseMove?.();
    if (pollHandle !== undefined) cancelAnimationFrame(pollHandle);
    document.removeEventListener("mousemove", onAnyMouseMove, true);
    document.removeEventListener("pointermove", onAnyMouseMove, true);
    document.removeEventListener("mouseover", onAnyMouseMove, true);
    document.removeEventListener("mouseleave", onLeaveDocument, true);
  });

  $effect(() => {
    void prompts;
    void armed;
    fitWindow();
  });

  const top5 = $derived(prompts.slice(0, 5));
</script>

<svelte:window on:keydown={onKey} />

<div
  class="root"
  bind:this={rootEl}
  onmousemove={updateHoverFromEvent}
  onmouseover={updateHoverFromEvent}
  onmouseleave={clearHover}
>
  <!-- Header — title + native toggle switch -->
  <div class="header">
    <span class="title">Prompt Player</span>
    <button
      class="switch"
      class:on={armed}
      role="switch"
      aria-checked={armed}
      onclick={toggleArmed}
    >
      <span class="knob"></span>
    </button>
  </div>

  <div class="sep"></div>

  {#if top5.length === 0}
    <div class="row disabled">
      <span class="label muted">No prompts</span>
    </div>
  {:else}
    {#each top5 as p (p.id)}
      <button
        class="row"
        class:hover={hoverKey === `p:${p.id}`}
        data-hkey={`p:${p.id}`}
        onclick={() => onPromptClick(p)}
      >
        <span class="check">{p.enabled ? "✓" : ""}</span>
        <span class="label">{p.name}</span>
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
    <span class="label">Prompt library…</span>
  </button>
  <button
    class="row plain"
    class:hover={hoverKey === "picker"}
    data-hkey="picker"
    onclick={() => action("picker")}
  >
    <span class="label">Command palette…</span>
  </button>
  <button
    class="row plain"
    class:hover={hoverKey === "settings"}
    data-hkey="settings"
    onclick={() => action("settings")}
  >
    <span class="label">Settings…</span>
  </button>

  <div class="sep"></div>

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
  </button>
</div>

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
    padding: 3px 6px;
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
    transition: background-color 60ms ease;
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

  .check {
    display: inline-block;
    width: 14px;
    flex-shrink: 0;
    text-align: center;
    font-size: 11px;
    font-weight: 700;
  }
  .row.plain .label {
    padding-left: 14px;
  }

  .label {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    margin-left: 4px;
  }

  .label.muted {
    color: rgba(255, 255, 255, 0.5);
    margin-left: 18px;
  }

  .sep {
    height: 1px;
    background: rgba(255, 255, 255, 0.13);
    margin: 4px 12px;
  }

  @media (prefers-color-scheme: light) {
    :global(html), :global(body), :global(#app) {
      color: rgba(0, 0, 0, 0.88);
    }
    .row.hover:not(.disabled) {
      background: rgba(0, 0, 0, 0.10);
    }
    .label.muted {
      color: rgba(0, 0, 0, 0.5);
    }
    .sep {
      background: rgba(0, 0, 0, 0.1);
    }
  }
</style>
