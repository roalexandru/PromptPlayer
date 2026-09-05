<script lang="ts">
  import { onMount, onDestroy, tick } from "svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import {
    ipc,
    fmtErr,
    isRemote,
    type Prompt,
    type SearchHit,
    type PromptStop,
    type PickMode,
  } from "$lib/ipc";
  import { IS_MAC } from "$lib/platform";

  let q = $state("");
  let hits: SearchHit[] = $state([]);
  let prompts = $state<Map<string, Prompt>>(new Map());
  let selected = $state(0);
  let inputEl = $state<HTMLInputElement | null>(null);
  let listEl = $state<HTMLUListElement | null>(null);
  // One-line delivery failure surfaced near the footer; the palette stays
  // open so the user can retry or pick a different prompt.
  let pickError = $state<string | null>(null);

  // §6.4 — choices resolve here, in the palette, before it dismisses. A modal
  // popup mid-expansion is a flow-killer, so when the selected prompt has tab
  // stops we swap the list for a compact inline form instead of firing with
  // the first option silently chosen.
  let stopPrompt = $state<Prompt | null>(null);
  let stops = $state<PromptStop[]>([]);
  let answers = $state<Record<string, string>>({});
  let stopMode = $state<PickMode>("paste");
  let stopIndex = $state(0);
  const resolving = $derived(stopPrompt !== null);

  async function loadPrompts() {
    const all = await ipc.listPrompts();
    prompts = new Map(all.map((p) => [p.id, p]));
  }

  // Monotonic request token: responses can arrive out of order while the
  // user types, and applying a stale response would show results for an
  // older query (and reset the selection against the wrong list).
  let searchSeq = 0;
  async function search() {
    const seq = ++searchSeq;
    const res = await ipc.pickerSearch(q, 50);
    if (seq !== searchSeq) return; // Stale — a newer query is in flight.
    hits = res;
    selected = 0;
    pickError = null;
  }

  async function pick(mode: PickMode) {
    const hit = hits[selected];
    if (!hit) return;
    pickError = null;
    const prompt = prompts.get(hit.prompt_id);
    try {
      // Ask about tab stops / choices first, if the body has any.
      const found = await ipc.promptStops(hit.prompt_id);
      if (found.length > 0 && prompt) {
        stopPrompt = prompt;
        stops = found;
        stopMode = mode;
        stopIndex = 0;
        answers = Object.fromEntries(
          found.map((s) => [s.key, s.options[0] ?? s.default ?? ""]),
        );
        return;
      }
    } catch (e) {
      // A stop-scan failure must not block delivery — fall through and fire
      // with no answers, which is the pre-resolver behaviour.
      console.warn("stop scan failed", e);
    }
    await deliver(hit.prompt_id, mode);
  }

  async function deliver(
    promptId: string,
    mode: PickMode,
    withAnswers?: Record<string, string>,
  ) {
    try {
      await ipc.pickerSelect(promptId, mode, withAnswers);
      cancelResolve();
    } catch (e) {
      // Keep the palette open — a silent unhandled rejection mid-demo is the
      // worst outcome. The user can retry or Esc out.
      pickError = `Couldn't deliver — ${fmtErr(e)}`;
    }
  }

  function cancelResolve() {
    stopPrompt = null;
    stops = [];
    answers = {};
    stopIndex = 0;
  }

  async function confirmStops() {
    if (!stopPrompt) return;
    await deliver(stopPrompt.id, stopMode, answers);
  }

  /** Cycle a choice stop's selection by `delta` (arrow keys in the resolver). */
  function cycleChoice(stop: PromptStop, delta: number) {
    if (stop.options.length === 0) return;
    const current = stop.options.indexOf(answers[stop.key] ?? "");
    const next =
      (current + delta + stop.options.length) % stop.options.length;
    answers = { ...answers, [stop.key]: stop.options[next] };
  }

  /** Human label for a delivery mode, used by the resolver's confirm button. */
  function modeLabel(m: PickMode): string {
    if (m === "fast") return "Type fast";
    if (m === "human") return "Type";
    if (m === "run") return "Type & send";
    return "Paste";
  }

  async function dismiss() {
    q = "";
    pickError = null;
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

  // For `custom` profiles, derive WPM from the iki-median-ms override:
  // ~5 chars/word → WPM ≈ 60000 / (iki × 5). Baseline 140 ms ≈ 85 wpm.
  function promptWpm(p: Prompt): number {
    const k = p.typing_profile ?? "sales-engineer";
    if (k === "custom") {
      const iki = p.typing_overrides?.["iki-median-ms"] ?? 140;
      return Math.max(1, 12000 / iki);
    }
    return profileWpm(k);
  }

  function estimateSeconds(p: Prompt): number {
    const words = p.body.trim().split(/\s+/).filter((w) => w.length).length;
    return Math.max(1, Math.round((words / promptWpm(p)) * 60));
  }

  // Split a prompt name into plain/highlighted runs from the backend's
  // match offsets. Offsets index the search haystack (name-first), so only
  // those < name.length apply here — the rest hit triggers/tags/etc.
  function nameSegments(
    name: string,
    highlights: number[],
  ): { text: string; hit: boolean }[] {
    const set = new Set(
      (highlights ?? []).filter((i) => i >= 0 && i < name.length),
    );
    if (set.size === 0) return [{ text: name, hit: false }];
    const segs: { text: string; hit: boolean }[] = [];
    let cur = "";
    let curHit = set.has(0);
    for (let i = 0; i < name.length; i++) {
      const hit = set.has(i);
      if (hit === curHit) {
        cur += name[i];
      } else {
        segs.push({ text: cur, hit: curHit });
        cur = name[i];
        curHit = hit;
      }
    }
    if (cur) segs.push({ text: cur, hit: curHit });
    return segs;
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
      if (resolving) {
        // Back to the list, not out of the palette — the user is mid-choice.
        cancelResolve();
        focusInput();
        return;
      }
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
      if (resolving) {
        stopIndex = Math.min(stopIndex + 1, Math.max(0, stops.length - 1));
        return;
      }
      // Clamp both ends — on an empty list `min(…, length - 1)` is -1.
      selected = Math.max(0, Math.min(selected + 1, hits.length - 1));
      ensureSelectedVisible();
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      if (resolving) {
        stopIndex = Math.max(0, stopIndex - 1);
        return;
      }
      selected = Math.max(0, Math.min(selected - 1, hits.length - 1));
      ensureSelectedVisible();
      return;
    }
    if (resolving && (e.key === "ArrowLeft" || e.key === "ArrowRight")) {
      const stop = stops[stopIndex];
      if (stop && stop.options.length > 0) {
        e.preventDefault();
        cycleChoice(stop, e.key === "ArrowRight" ? 1 : -1);
      }
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      // Stop propagation so other listeners (and Win32 default `Enter` =
      // form-submit semantics, which on Windows can race the modifier read)
      // can't override our mode classification.
      e.stopPropagation();
      if (resolving) {
        confirmStops();
        return;
      }
      // §5.3: the primary modifier means "type it and submit" — the mode the
      // spec listed and the picker never had. It is also what makes this
      // useful against a coding agent, where "typed but not sent" is half a
      // turn.
      const primary = IS_MAC ? e.metaKey : e.ctrlKey;
      if (primary) pick("run");
      else if (e.shiftKey) pick("fast");
      else if (e.altKey) pick("human");
      else pick("paste");
      return;
    }
    const primaryDigit = IS_MAC ? e.metaKey : e.ctrlKey;
    if (e.key >= "1" && e.key <= "9" && primaryDigit) {
      e.preventDefault();
      const idx = parseInt(e.key) - 1;
      if (idx < hits.length) {
        selected = idx;
        // Match the default Enter binding so quick-select keys and a plain
        // Enter both feel like "just deliver this now".
        pick("paste");
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

  // Capture-phase keydown handler at document level. The previous
  // `<svelte:window on:keydown>` worked on Mac but Alt+Enter never fired
  // its handler on Windows. Capturing on `document` runs the listener
  // before WebView2's bubble-phase processing of system-key events
  // (WM_SYSKEYDOWN, which is what tao routes Alt-modified keystrokes
  // through), so we see Alt+Enter regardless of any later handler that
  // might consume it.
  function onKeyCapture(e: KeyboardEvent) {
    onKey(e);
  }

  onMount(async () => {
    document.addEventListener("keydown", onKeyCapture, true);
    await loadPrompts();
    // No explicit search() here — the `$effect` above runs once on mount
    // (and again on every q change), so calling it here too would fire a
    // duplicate initial query.
    await focusInput();
    // Refocus + clear query each time the picker is shown so subsequent
    // open cycles start fresh with the input ready.
    unlistenShown = await listen("picker-shown", async () => {
      await loadPrompts();
      q = "";
      selected = 0;
      // Run search() explicitly: the reactive `$effect` only re-runs when
      // `q` actually changes, so if the user dismissed the previous open
      // with q already empty (the common case), the effect would no-op
      // and `hits` would stay frozen at its last value. That's the empty
      // "Nothing here yet" bug — first open happened before any prompts
      // existed, every subsequent open kept the cached empty array.
      await search();
      // Yield to the microtask queue so the q="" reactive update + search
      // effect have flushed before we start the focus poll. Otherwise
      // focus could be stolen by the list re-render that follows.
      await Promise.resolve();
      await tick();
      await focusInput();
    });
  });

  onDestroy(() => {
    document.removeEventListener("keydown", onKeyCapture, true);
    unlistenShown?.();
  });
</script>

<div class="root" class:opaque={!IS_MAC}>
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

  {#if resolving && stopPrompt}
    <div class="resolver">
      <div class="resolver-head">
        <span class="resolver-title">{stopPrompt.name}</span>
        <span class="resolver-hint">
          {stops.length === 1 ? "1 value" : `${stops.length} values`} to fill
        </span>
      </div>
      {#each stops as stop, i (stop.key)}
        <div class="stop" class:current={i === stopIndex}>
          <span class="stop-key">${stop.key}</span>
          {#if stop.options.length > 0}
            <div class="stop-options" role="radiogroup" aria-label={`Choice ${stop.key}`}>
              {#each stop.options as opt (opt)}
                <button
                  type="button"
                  class="chip"
                  class:sel={answers[stop.key] === opt}
                  aria-pressed={answers[stop.key] === opt}
                  onclick={() => { answers = { ...answers, [stop.key]: opt }; stopIndex = i; }}
                >{opt}</button>
              {/each}
            </div>
          {:else}
            <input
              class="stop-input"
              value={answers[stop.key] ?? ""}
              placeholder={stop.default ?? "value"}
              oninput={(e) => {
                answers = { ...answers, [stop.key]: e.currentTarget.value };
                stopIndex = i;
              }}
            />
          {/if}
        </div>
      {/each}
      <div class="resolver-actions">
        <button type="button" class="confirm" onclick={confirmStops}>
          {modeLabel(stopMode)}
        </button>
        <button type="button" class="cancel" onclick={() => { cancelResolve(); focusInput(); }}>
          Back
        </button>
      </div>
    </div>
  {:else}
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
            onclick={() => { selected = i; pick("paste"); }}
            onmousemove={() => (selected = i)}
          >
            <span class="row-left">
              <!-- Single line: whitespace between blocks would render as
                   stray spaces inside the name. -->
              <span class="row-name">{#each nameSegments(p.name, h.highlights) as seg}{#if seg.hit}<mark class="hl">{seg.text}</mark>{:else}{seg.text}{/if}{/each}</span>
              {#if p.description}
                <span class="row-desc">{p.description}</span>
              {/if}
            </span>
            <span class="row-right">
              {#if !p.enabled}<span class="badge off">off</span>{/if}
              {#if isRemote(p)}<span class="badge remote" title="From a remote source — read-only">shared</span>{/if}
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
  {/if}

  {#if pickError}
    <div class="pick-error" role="alert">{pickError}</div>
  {/if}

  <footer>
    {#if resolving}
      <span><kbd>↑</kbd><kbd>↓</kbd> field</span>
      <span><kbd>←</kbd><kbd>→</kbd> choice</span>
      <span><kbd>↵</kbd> {modeLabel(stopMode).toLowerCase()}</span>
      <span class="grow"></span>
      <span><kbd>esc</kbd> back</span>
    {:else}
      <span><kbd>↑</kbd><kbd>↓</kbd> navigate</span>
      <span><kbd>↵</kbd> paste</span>
      {#if IS_MAC}
        <span><kbd>⇧↵</kbd> fast</span>
        <span><kbd>⌥↵</kbd> type</span>
        <span><kbd>⌘↵</kbd> send</span>
      {:else}
        <span><kbd>Shift+↵</kbd> fast</span>
        <span><kbd>Alt+↵</kbd> type</span>
        <span><kbd>Ctrl+↵</kbd> send</span>
      {/if}
      <span class="grow"></span>
      <span><kbd>esc</kbd></span>
    {/if}
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
  /* Windows fallback: WebView2 on Win11 24H2 sometimes ignores Tauri's
     `transparent: true` and paints solid white, and apply_mica can no-op
     in light-mode + decorationless setups — either way the white-on-
     transparent CSS goes invisible. Paint our own near-opaque dark
     surface so the palette is readable regardless. Mica still composites
     when it works; this is a safety net, not a replacement. */
  .root.opaque {
    background: rgba(28, 28, 30, 0.96);
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
  /* Search-match highlight — accent color only (same weight/metrics, so no
     layout shift). Underline keeps it visible on the blue active row. */
  .row-name mark.hl {
    background: transparent;
    color: rgba(120, 175, 255, 1);
    font-weight: inherit;
    text-decoration: underline;
    text-decoration-color: rgba(120, 175, 255, 0.6);
    text-underline-offset: 2px;
  }
  .list li.active .row-name mark.hl {
    color: #fff;
    text-decoration-color: rgba(255, 255, 255, 0.7);
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

  /* Delivery failure — one-line inline error pinned above the footer. */
  .pick-error {
    padding: 6px 14px;
    border-top: 0.5px solid rgba(255, 99, 92, 0.35);
    background: rgba(255, 69, 58, 0.14);
    color: rgba(255, 150, 145, 0.95);
    font-size: 11px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
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

  /* Inline stop resolver (§6.4) — deliberately compact so it reads as part of
     the palette rather than a dialog on top of it. */
  .resolver {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px 12px 12px;
    overflow-y: auto;
    flex: 1;
  }
  .resolver-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
  }
  .resolver-title {
    font-weight: 600;
  }
  .resolver-hint {
    font-size: 11px;
    opacity: 0.6;
  }
  .stop {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 6px;
    border-radius: 6px;
    border: 1px solid transparent;
  }
  .stop.current {
    border-color: rgba(255, 255, 255, 0.22);
    background: rgba(255, 255, 255, 0.06);
  }
  .stop-key {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 11px;
    opacity: 0.7;
    min-width: 22px;
  }
  .stop-options {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chip {
    border: 1px solid rgba(255, 255, 255, 0.2);
    background: transparent;
    color: inherit;
    border-radius: 999px;
    padding: 2px 10px;
    font-size: 12px;
    cursor: pointer;
  }
  .chip.sel {
    background: rgba(255, 255, 255, 0.9);
    color: #16161a;
    border-color: transparent;
  }
  .stop-input {
    flex: 1;
    background: rgba(0, 0, 0, 0.25);
    border: 1px solid rgba(255, 255, 255, 0.16);
    border-radius: 6px;
    color: inherit;
    padding: 4px 8px;
    font-size: 12px;
  }
  .resolver-actions {
    display: flex;
    gap: 8px;
    margin-top: 2px;
  }
  .resolver-actions button {
    border-radius: 6px;
    padding: 5px 12px;
    font-size: 12px;
    cursor: pointer;
    border: 1px solid rgba(255, 255, 255, 0.2);
    background: transparent;
    color: inherit;
  }
  .resolver-actions .confirm {
    background: rgba(255, 255, 255, 0.9);
    color: #16161a;
    border-color: transparent;
    font-weight: 600;
  }
  .badge.remote {
    background: rgba(120, 170, 255, 0.22);
  }
</style>
