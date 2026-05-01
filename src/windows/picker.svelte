<script lang="ts">
  import { onMount, onDestroy, tick } from "svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { ipc, type Prompt, type SearchHit } from "$lib/ipc";
  import { IS_MAC } from "$lib/platform";

  let q = $state("");
  let hits: SearchHit[] = $state([]);
  let prompts = $state<Map<string, Prompt>>(new Map());
  let selected = $state(0);
  let inputEl = $state<HTMLInputElement | null>(null);
  let listEl = $state<HTMLUListElement | null>(null);

  async function loadPrompts() {
    const all = await ipc.listPrompts();
    prompts = new Map(all.map((p) => [p.id, p]));
  }

  async function search() {
    hits = await ipc.pickerSearch(q, 50);
    selected = 0;
  }

  async function pick(mode: "human" | "fast" | "paste" | "run") {
    const hit = hits[selected];
    if (!hit) return;
    await ipc.pickerSelect(hit.prompt_id, mode);
  }

  async function dismiss() {
    q = "";
    // Hide AND restore focus to the previously-foreground app (so the next
    // user keystroke goes there, not nowhere). Backend handles both.
    await ipc.pickerDismiss();
  }

  function profileShort(k: string): string {
    if (k === "fast-presenter") return "Fast";
    if (k === "thoughtful-ceo") return "CEO";
    if (k === "custom") return "Custom";
    return "SE";
  }

  function profileWpm(k: string): number {
    if (k === "fast-presenter") return 100;
    if (k === "thoughtful-ceo") return 60;
    return 80;
  }

  function estimateSeconds(p: Prompt): number {
    const words = p.body.trim().split(/\s+/).filter((w) => w.length).length;
    return Math.max(1, Math.round((words / profileWpm(p.typing_profile ?? "sales-engineer")) * 60));
  }

  async function ensureSelectedVisible() {
    await tick();
    if (!listEl) return;
    const item = listEl.querySelector<HTMLLIElement>(`li[data-i="${selected}"]`);
    if (!item) return;
    const lr = listEl.getBoundingClientRect();
    const ir = item.getBoundingClientRect();
    if (ir.top < lr.top) listEl.scrollTop -= lr.top - ir.top;
    else if (ir.bottom > lr.bottom) listEl.scrollTop += ir.bottom - lr.bottom;
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      // Two-step Esc: first press clears the search if non-empty, second
      // press closes the palette. Mirrors Spotlight / Raycast behavior.
      if (q.length > 0) {
        q = "";
        selected = 0;
        // Re-anchor focus after the input clears.
        focusInput();
      } else {
        dismiss();
      }
      return;
    }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      selected = Math.min(selected + 1, hits.length - 1);
      ensureSelectedVisible();
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      selected = Math.max(selected - 1, 0);
      ensureSelectedVisible();
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      // Primary modifier for "run": Cmd on Mac, Ctrl on Windows. Win key
      // (e.metaKey on Windows) intentionally not accepted — that's the OS,
      // not the user's "run this" intent.
      const primary = IS_MAC ? e.metaKey : e.ctrlKey;
      if (e.shiftKey) pick("fast");
      else if (e.altKey) pick("paste");
      else if (primary) pick("run");
      else pick("human");
      return;
    }
    const primaryDigit = IS_MAC ? e.metaKey : e.ctrlKey;
    if (e.key >= "1" && e.key <= "9" && primaryDigit) {
      e.preventDefault();
      const idx = parseInt(e.key) - 1;
      if (idx < hits.length) {
        selected = idx;
        pick("human");
      }
    }
  }

  $effect(() => {
    void q;
    search();
  });

  let unlistenShown: UnlistenFn | null = null;

  let focusPollHandle: ReturnType<typeof setInterval> | null = null;

  async function focusInput() {
    await tick();
    if (!inputEl) return;
    if (focusPollHandle) {
      clearInterval(focusPollHandle);
      focusPollHandle = null;
    }
    // Retry focus until the OS (not just the DOM) actually has focus on
    // our input. The race: `makeKeyWindow` returns before AppKit's run-loop
    // drains the key transition, so a one-shot focus() can land while the
    // panel is still not really key, leaving keystrokes routed to the
    // previously-active app — even though `document.activeElement` looks
    // correct. The exit condition must be `document.hasFocus() &&
    // activeElement === inputEl`. setInterval (not rAF) so the loop ticks
    // even while the panel is alpha-0 / not yet composited.
    const start = performance.now();
    const tryGrab = () => {
      if (!inputEl) {
        if (focusPollHandle) {
          clearInterval(focusPollHandle);
          focusPollHandle = null;
        }
        return;
      }
      inputEl.focus();
      inputEl.select?.();
      const ok = document.hasFocus() && document.activeElement === inputEl;
      if (ok || performance.now() - start > 1500) {
        if (focusPollHandle) {
          clearInterval(focusPollHandle);
          focusPollHandle = null;
        }
      }
    };
    tryGrab();
    focusPollHandle = setInterval(tryGrab, 30);
  }

  onMount(async () => {
    await loadPrompts();
    await search();
    await focusInput();
    // Refocus + clear query each time the picker is shown so subsequent
    // open cycles start fresh with the input ready.
    unlistenShown = await listen("picker-shown", async () => {
      await loadPrompts();
      q = "";
      selected = 0;
      // Yield to the microtask queue so the q="" reactive update + search
      // effect have flushed before we start the focus poll. Otherwise
      // focus could be stolen by the list re-render that follows.
      await Promise.resolve();
      await tick();
      await focusInput();
    });
  });

  onDestroy(() => {
    unlistenShown?.();
  });
</script>

<svelte:window on:keydown={onKey} />

<div class="root">
  <div class="search">
    <span class="icon" aria-hidden="true">⌕</span>
    <input
      bind:this={inputEl}
      bind:value={q}
      placeholder="Search prompts…"
      autocomplete="off"
      autocorrect="off"
      spellcheck="false"
    />
    {#if q}
      <button class="clear" onclick={() => (q = "")} aria-label="Clear">×</button>
    {/if}
  </div>

  <ul class="list" bind:this={listEl}>
    {#each hits as h, i (h.prompt_id)}
      {@const p = prompts.get(h.prompt_id)}
      {#if p}
        <li
          data-i={i}
          class:active={i === selected}
          class:disabled={!p.enabled}
        >
          <button
            onclick={() => { selected = i; pick("human"); }}
            onmousemove={() => (selected = i)}
          >
            <span class="row-left">
              <span class="row-name">{p.name}</span>
              {#if p.description}
                <span class="row-desc">{p.description}</span>
              {/if}
            </span>
            <span class="row-right">
              {#if !p.enabled}<span class="badge off">off</span>{/if}
              <span class="badge profile">{profileShort(p.typing_profile ?? "sales-engineer")}</span>
              <span class="badge time">~{estimateSeconds(p)}s</span>
              {#if p.triggers.length}
                <code class="trigger">{p.triggers[0]}{p.commit_char}</code>
              {/if}
            </span>
          </button>
        </li>
      {/if}
    {/each}
    {#if hits.length === 0}
      <li class="empty">
        {#if q}No matches{:else}Nothing here yet{/if}
      </li>
    {/if}
  </ul>

  <footer>
    <span><kbd>↑</kbd><kbd>↓</kbd> navigate</span>
    <span><kbd>↵</kbd> type</span>
    {#if IS_MAC}
      <span><kbd>⇧↵</kbd> fast</span>
      <span><kbd>⌥↵</kbd> paste</span>
      <span><kbd>⌘↵</kbd> run</span>
    {:else}
      <span><kbd>Shift+↵</kbd> fast</span>
      <span><kbd>Alt+↵</kbd> paste</span>
      <span><kbd>Ctrl+↵</kbd> run</span>
    {/if}
    <span class="grow"></span>
    <span><kbd>esc</kbd></span>
  </footer>
</div>

<style>
  :global(html), :global(body), :global(#app) {
    margin: 0;
    padding: 0;
    background: transparent !important;
    height: 100%;
    overflow: hidden;
    color: rgba(255, 255, 255, 0.92);
    font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text",
      "Segoe UI", sans-serif;
    font-size: 13px;
    -webkit-font-smoothing: antialiased;
  }

  .root {
    display: flex;
    flex-direction: column;
    height: 100vh;
    box-sizing: border-box;
    padding: 0;
  }

  /* Search bar — large, Raycast-style, transparent so vibrancy shows. */
  .search {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 16px;
    border-bottom: 0.5px solid rgba(255, 255, 255, 0.08);
  }
  .search .icon {
    color: rgba(255, 255, 255, 0.45);
    font-size: 16px;
    line-height: 1;
  }
  .search input {
    flex: 1;
    background: transparent;
    border: none;
    outline: none;
    color: inherit;
    font-size: 16px;
    font-weight: 400;
    letter-spacing: -0.1px;
    padding: 0;
  }
  .search input::placeholder {
    color: rgba(255, 255, 255, 0.42);
  }
  .search .clear {
    background: rgba(255, 255, 255, 0.10);
    border: none;
    color: rgba(255, 255, 255, 0.55);
    width: 18px;
    height: 18px;
    line-height: 16px;
    border-radius: 50%;
    cursor: default;
    font-size: 13px;
    padding: 0;
  }
  .search .clear:hover { background: rgba(255, 255, 255, 0.18); color: #fff; }

  /* Result list — single column, hover/keyboard-driven selection. */
  .list {
    list-style: none;
    margin: 0;
    padding: 6px;
    overflow-y: auto;
    flex: 1;
    min-height: 0;
  }
  .list::-webkit-scrollbar { width: 6px; }
  .list::-webkit-scrollbar-thumb {
    background: rgba(255, 255, 255, 0.15);
    border-radius: 3px;
  }
  .list li {
    margin: 0;
    padding: 0;
  }
  .list li button {
    display: flex;
    align-items: center;
    width: 100%;
    background: none;
    border: none;
    color: inherit;
    text-align: left;
    padding: 7px 10px;
    border-radius: 6px;
    cursor: default;
    user-select: none;
    -webkit-user-select: none;
  }
  .list li.active button {
    background: rgba(0, 122, 255, 0.85);
    color: #fff;
  }
  .list li.disabled button .row-name { opacity: 0.55; }
  .list li.empty {
    padding: 28px 16px;
    text-align: center;
    color: rgba(255, 255, 255, 0.42);
    font-size: 13px;
  }

  .row-left {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-width: 0;
    gap: 2px;
  }
  .row-name {
    font-size: 13px;
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .row-desc {
    font-size: 11px;
    color: rgba(255, 255, 255, 0.5);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .list li.active .row-desc { color: rgba(255, 255, 255, 0.78); }

  .row-right {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-left: 12px;
    flex-shrink: 0;
  }
  .badge {
    font-size: 10px;
    padding: 2px 6px;
    border-radius: 999px;
    background: rgba(255, 255, 255, 0.1);
    color: rgba(255, 255, 255, 0.72);
    font-weight: 500;
    line-height: 1.2;
  }
  .badge.off {
    background: rgba(255, 80, 80, 0.18);
    color: rgba(255, 145, 145, 0.95);
  }
  .list li.active .badge {
    background: rgba(255, 255, 255, 0.22);
    color: #fff;
  }

  .trigger {
    font-family: ui-monospace, "SF Mono", monospace;
    font-size: 11px;
    padding: 2px 6px;
    border-radius: 4px;
    background: rgba(10, 132, 255, 0.18);
    color: rgba(120, 175, 255, 1);
  }
  .list li.active .trigger {
    background: rgba(255, 255, 255, 0.22);
    color: #fff;
  }

  /* Footer — keyboard hints. */
  footer {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 7px 14px;
    border-top: 0.5px solid rgba(255, 255, 255, 0.08);
    font-size: 11px;
    color: rgba(255, 255, 255, 0.55);
  }
  footer .grow { flex: 1; }
  kbd {
    display: inline-block;
    background: rgba(255, 255, 255, 0.1);
    padding: 1px 5px;
    border-radius: 3px;
    font-family: ui-monospace, "SF Mono", monospace;
    font-size: 10px;
    line-height: 1.5;
    color: rgba(255, 255, 255, 0.78);
    margin-right: 3px;
  }
</style>
