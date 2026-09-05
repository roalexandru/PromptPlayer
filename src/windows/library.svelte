<script lang="ts">
  import { onMount } from "svelte";
  import {
    ipc,
    fmtErr,
    isRemote,
    setEnabled,
    type Prompt,
    type ProfileKind,
    type NewlineMode,
  } from "$lib/ipc";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onWindowShown, LIBRARY_CHANGED, ARMED_CHANGED } from "$lib/events";
  import { open as openFileDialog, save as saveFileDialog, confirm } from "@tauri-apps/plugin-dialog";
  import { IS_MAC } from "$lib/platform";
  import CadencePreview from "$lib/components/CadencePreview.svelte";
  import HotkeyRecorder from "$lib/components/HotkeyRecorder.svelte";
  import CompanionPanel from "$lib/components/CompanionPanel.svelte";

  const NEW_HINT = IS_MAC ? "⌘N" : "Ctrl+N";

  let prompts: Prompt[] = $state([]);
  let selectedId = $state<string | null>(null);
  let armed = $state(false);
  let error = $state<string | null>(null);
  let tab: "edit" | "preview" = $state("edit");
  // Companion settings (sources, setlist, imports) live in their own view so
  // the prompt editor stays uncluttered.
  let view: "prompts" | "companion" = $state("prompts");

  // Local working copy of the selected prompt (the one we're editing).
  let draft = $state<Prompt | null>(null);
  // A prompt from a remote source is read-only: its cache is replaced on every
  // refresh, so an edit would silently vanish. The UI offers a fork instead.
  const draftIsRemote = $derived(draft !== null && isRemote(draft));
  let dirty = $state(false);
  let saveStatus = $state<"" | "saving" | "saved" | "error">("");
  // Validity surfaced by the HotkeyRecorder (conflict / unparseable combo).
  // Non-null blocks Save so a broken hotkey never reaches the backend.
  let hotkeyError = $state<string | null>(null);

  // Transient success banner (export feedback etc) — mirrors the error
  // banner but auto-dismisses.
  let notice = $state<string | null>(null);
  let noticeTimer: ReturnType<typeof setTimeout> | null = null;
  function flashNotice(msg: string) {
    notice = msg;
    if (noticeTimer) clearTimeout(noticeTimer);
    noticeTimer = setTimeout(() => (notice = null), 4000);
  }

  // Sidebar filter — client-side substring match over name, triggers, tags.
  let filter = $state("");
  let visiblePrompts = $derived.by(() => {
    const q = filter.trim().toLowerCase();
    if (!q) return prompts;
    return prompts.filter(
      (p) =>
        p.name.toLowerCase().includes(q) ||
        p.triggers.some((t) => t.toLowerCase().includes(q)) ||
        (p.tags ?? []).some((t) => t.toLowerCase().includes(q)),
    );
  });

  // Library folder path — surfaced in the sidebar footer with click-to-copy.
  let libRoot = $state<string>("");
  let copiedRoot = $state(false);
  let copiedRootTimer: ReturnType<typeof setTimeout> | null = null;
  async function copyLibRoot() {
    if (!libRoot) return;
    try {
      await navigator.clipboard.writeText(libRoot);
      copiedRoot = true;
      if (copiedRootTimer) clearTimeout(copiedRootTimer);
      copiedRootTimer = setTimeout(() => (copiedRoot = false), 1500);
    } catch {}
  }

  let charCount = $derived(draft?.body.length ?? 0);
  let wordCount = $derived(
    draft
      ? draft.body.trim().split(/\s+/).filter((w) => w.length > 0).length
      : 0,
  );
  // Per-profile WPM matches what the Rust engine actually delivers (see profiles.rs).
  function profileWpm(k: string): number {
    if (k === "fast-presenter") return 100;
    if (k === "thoughtful-ceo") return 60;
    return 80;
  }
  // For `custom` profiles the WPM follows the iki-median-ms override:
  // ~5 chars/word → WPM ≈ 60000 / (iki × 5). Baseline 140 ms ≈ 85 wpm.
  function draftWpm(d: Prompt): number {
    const k = d.typing_profile ?? "sales-engineer";
    if (k === "custom") {
      const iki = d.typing_overrides?.["iki-median-ms"] ?? 140;
      return Math.max(1, 12000 / iki);
    }
    return profileWpm(k);
  }
  let estSeconds = $derived(
    draft ? Math.round((wordCount / draftWpm(draft)) * 60 + 1.0) : 0,
  );

  // Frontmatter fields are optional on disk, so the generated TS type is too.
  // These helpers are the only place that resolves the defaults.
  const EMPTY_OVERRIDES = {} as NonNullable<Prompt["typing_overrides"]>;
  function tov(p: Prompt | null) {
    return p?.typing_overrides ?? EMPTY_OVERRIDES;
  }
  function ensureTov(p: Prompt) {
    if (!p.typing_overrides) p.typing_overrides = { ...EMPTY_OVERRIDES };
    return p.typing_overrides;
  }
  function positiveOrNull(value: string): number | null {
    if (value === "") return null;
    const n = Number(value);
    return Number.isFinite(n) && n > 0 ? n : null;
  }
  function rateOrNull(value: string): number | null {
    if (value === "") return null;
    const n = Number(value);
    return Number.isFinite(n) ? Math.min(1, Math.max(0, n)) : null;
  }

  async function refresh() {
    try {
      const fresh = await ipc.listPrompts();
      prompts = fresh;
      if (!selectedId && fresh.length) {
        selectedId = fresh[0].id;
        draft = clone(fresh[0]);
        dirty = false;
      } else if (selectedId && !dirty) {
        // Refresh from server only when user has no unsaved edits.
        const updated = fresh.find((p) => p.id === selectedId);
        if (updated) draft = clone(updated);
      }
    } catch (e) {
      error = fmtErr(e);
    }
  }

  function clone(p: Prompt): Prompt {
    return JSON.parse(JSON.stringify(p));
  }

  async function refreshArmed() {
    try { armed = await ipc.getArmed(); } catch {}
  }
  async function toggleArmed() { armed = await ipc.toggleArmed(); }

  async function selectPrompt(p: Prompt) {
    if (dirty && !(await confirm("Discard unsaved changes?"))) return;
    selectedId = p.id;
    draft = clone(p);
    dirty = false;
    saveStatus = "";
  }

  function markDirty() {
    dirty = true;
    saveStatus = "";
  }

  async function save() {
    if (!draft || hotkeyError) return;
    saveStatus = "saving";
    try {
      await ipc.savePrompt(draft);
      saveStatus = "saved";
      dirty = false;
      await refresh();
    } catch (e) {
      error = fmtErr(e);
      saveStatus = "error";
    }
  }

  async function createNew() {
    if (dirty && !(await confirm("Discard unsaved changes?"))) return;
    try {
      const p = await ipc.createPrompt();
      await refresh();
      selectedId = p.id;
      draft = clone(p);
      dirty = false;
    } catch (e) {
      error = fmtErr(e);
    }
  }

  async function togglePromptEnabled(p: Prompt, e: Event) {
    e.stopPropagation();
    e.preventDefault();
    const next = !p.enabled;
    // Optimistic update for snap UI; backend persists + hot-reload syncs.
    prompts = prompts.map((x) => (x.id === p.id ? { ...x, enabled: next } : x));
    if (draft && draft.id === p.id) draft = { ...draft, enabled: next };
    try {
      await setEnabled(p, next);
    } catch (err) {
      error = fmtErr(err);
      // Roll back.
      prompts = prompts.map((x) => (x.id === p.id ? { ...x, enabled: !next } : x));
      if (draft && draft.id === p.id) draft = { ...draft, enabled: !next };
    }
  }

  async function toggleDraftEnabled() {
    if (!draft) return;
    await togglePromptEnabled(draft, new Event("synth"));
  }

  async function togglePromptPinned(p: Prompt, e: Event) {
    e.stopPropagation();
    e.preventDefault();
    const next = !p.pinned;
    prompts = prompts.map((x) => (x.id === p.id ? { ...x, pinned: next } : x));
    if (draft && draft.id === p.id) draft = { ...draft, pinned: next };
    try {
      await ipc.setPromptPinned(p.id, next);
    } catch (err) {
      error = fmtErr(err);
      prompts = prompts.map((x) => (x.id === p.id ? { ...x, pinned: !next } : x));
      if (draft && draft.id === p.id) draft = { ...draft, pinned: !next };
    }
  }

  async function toggleDraftPinned() {
    if (!draft) return;
    await togglePromptPinned(draft, new Event("synth"));
  }

  async function forkCurrent() {
    if (!draft) return;
    try {
      const forked = await ipc.forkPrompt(draft.id);
      await refresh();
      selectedId = forked.id;
      draft = { ...forked };
      dirty = false;
      flashNotice(`Forked into your library as “${forked.name}” (disabled until you review it)`);
    } catch (e) {
      error = fmtErr(e);
    }
  }

  async function deleteCurrent() {
    if (!draft) return;
    if (!(await confirm(`Delete "${draft.name}"? This removes the .pp.md file.`))) return;
    try {
      await ipc.deletePrompt(draft.id);
      selectedId = null;
      draft = null;
      dirty = false;
      await refresh();
    } catch (e) {
      error = fmtErr(e);
    }
  }

  function profileLabel(k: string): string {
    if (k === "fast-presenter") return "Fast Presenter (~100 wpm)";
    if (k === "thoughtful-ceo") return "Thoughtful CEO (~60 wpm)";
    if (k === "custom") return "Custom";
    return "Sales Engineer (~80 wpm)";
  }

  // Comma-separated list <-> array helpers.
  function listToString(arr: string[]): string { return arr.join(", "); }
  function stringToList(s: string): string[] {
    return s.split(",").map((x) => x.trim()).filter((x) => x.length > 0);
  }

  // Manual `startDragging()` on mousedown — the declarative drag regions are
  // flaky under transparent + Overlay + macOSPrivateApi.
  async function startDrag(e: MouseEvent) {
    const target = e.target as HTMLElement;
    if (!target.closest(".ribbon")) return;
    if (target.closest("button, input, select, textarea, .topbar-actions, .arm-btn"))
      return;
    e.preventDefault();
    try {
      await getCurrentWindow().startDragging();
    } catch (err) {
      console.error("startDragging failed:", err);
    }
  }

  // Cmd/Ctrl+S save, Cmd/Ctrl+N new (matches the NEW_HINT tooltip).
  function onKey(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key === "s") {
      e.preventDefault();
      if (dirty) save();
      return;
    }
    if ((e.metaKey || e.ctrlKey) && e.key === "n") {
      e.preventDefault();
      createNew();
    }
  }

  // Sidebar keyboard navigation — ArrowUp/Down walk the (filtered) list,
  // Enter re-selects. Attached to the filter input and the list rows.
  function moveSelection(delta: number) {
    if (visiblePrompts.length === 0) return;
    const idx = visiblePrompts.findIndex((p) => p.id === selectedId);
    const next =
      idx === -1
        ? delta > 0 ? 0 : visiblePrompts.length - 1
        : Math.max(0, Math.min(visiblePrompts.length - 1, idx + delta));
    selectPrompt(visiblePrompts[next]);
  }
  function onSidebarKey(e: KeyboardEvent) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      moveSelection(1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      moveSelection(-1);
    } else if (e.key === "Enter") {
      e.preventDefault();
      const hit =
        visiblePrompts.find((p) => p.id === selectedId) ?? visiblePrompts[0];
      if (hit) selectPrompt(hit);
    }
  }

  // §10.2 helpers — scope auto-capture, expression Test, import/export.

  // Scope editor: bundle-ID chips and the window-title regex. URL-regex and
  // time-of-day stay frontmatter-only.
  function ensureScope(p: Prompt) {
    if (!p.scope) p.scope = { app: [], "window-title-regex": null, "url-regex": null, "time-of-day": null };
    return p.scope;
  }
  function addScopeApp(bundleId: string) {
    if (!draft || !bundleId.trim()) return;
    const s = ensureScope(draft);
    if (!s.app.includes(bundleId)) {
      s.app = [...s.app, bundleId];
      markDirty();
    }
  }
  function removeScopeApp(bundleId: string) {
    if (!draft?.scope) return;
    draft.scope = { ...draft.scope, app: draft.scope.app.filter((b) => b !== bundleId) };
    markDirty();
  }

  // Hide, count down 3s while the user switches apps, capture, re-show.
  // Capturing ourselves means they didn't switch in time — toast and drop it.
  let capturing = $state(false);
  let captureCountdown = $state(3);
  let captureMsg = $state<string | null>(null);
  async function captureScope() {
    if (!draft || capturing) return;
    capturing = true;
    captureMsg = null;
    try {
      const win = getCurrentWindow();
      await win.hide();
      for (let i = 3; i > 0; i--) {
        captureCountdown = i;
        await new Promise((r) => setTimeout(r, 1000));
      }
      const fg = await ipc.captureForegroundApp();
      await win.show();
      await win.setFocus();
      const id = fg.bundleId ?? fg.executable ?? null;
      if (!id) {
        captureMsg = "Could not identify foreground app.";
      } else if (id.includes("promptplayer") || id.includes("prompt-player") || id.toLowerCase().includes("prompt player")) {
        captureMsg = "Captured Prompt Player itself — switch to a different app first.";
      } else {
        addScopeApp(id);
        captureMsg = `Added: ${fg.name ?? id}`;
      }
    } catch (e) {
      captureMsg = `Capture failed: ${fmtErr(e)}`;
      try { await getCurrentWindow().show(); } catch {}
    } finally {
      capturing = false;
    }
  }

  // Expressions and placeholders were never used in the field, and there was
  // nowhere in the app that listed them. Click-to-insert reference.
  let refOpen = $state(false);
  const SNIPPET_GROUPS = [
    {
      title: "Tab stops",
      items: [
        { label: "$1", insert: "$1", help: "First tab stop" },
        { label: "$0", insert: "$0", help: "Final cursor position" },
        { label: "${1:default}", insert: "${1:default}", help: "Tab stop with default text" },
        { label: "${1|a,b|}", insert: "${1|a,b|}", help: "Choice, resolved in the picker" },
      ],
    },
    {
      title: "Variables",
      items: [
        { label: "$CLIPBOARD", insert: "$CLIPBOARD", help: "Current clipboard text" },
        { label: "$SELECTION", insert: "$SELECTION", help: "Selected text" },
        { label: "$DATE", insert: "$DATE", help: "Today, YYYY-MM-DD" },
        { label: "$TIME", insert: "$TIME", help: "Current time" },
        { label: "$APP_NAME", insert: "$APP_NAME", help: "Foreground app name" },
        { label: "$WINDOW_TITLE", insert: "$WINDOW_TITLE", help: "Foreground window title" },
        { label: "$UUID", insert: "$UUID", help: "Random UUID" },
      ],
    },
    {
      title: "Expressions",
      items: [
        { label: "today", insert: "${{ today }}", help: "Sandboxed JS — today's date" },
        { label: "now", insert: "${{ now.toISOString() }}", help: "ISO timestamp" },
        {
          label: "format_date",
          insert: '${{ format_date(now, "%Y-%m-%d") }}',
          help: "Format a date",
        },
        {
          label: "random_choice",
          insert: '${{ random_choice(["a", "b"]) }}',
          help: "Pick one at random",
        },
        { label: "app.name", insert: "${{ app.name }}", help: "Foreground app, in JS" },
      ],
    },
  ];

  // Insert at the caret so the reference composes with what's already typed.
  function insertSnippet(text: string) {
    if (!draft) return;
    const el = document.getElementById("prompt-body") as HTMLTextAreaElement | null;
    const body = draft.body ?? "";
    const start = el?.selectionStart ?? body.length;
    const end = el?.selectionEnd ?? body.length;
    draft.body = body.slice(0, start) + text + body.slice(end);
    markDirty();
    // Caret after the inserted text, once Svelte has written the new value.
    queueMicrotask(() => {
      if (!el) return;
      el.focus();
      const caret = start + text.length;
      el.setSelectionRange(caret, caret);
    });
  }

  // Run the body through expressions + placeholders so authors can check a
  // `${{ … }}` block without firing for real.
  let testOpen = $state(false);
  let testResult = $state<string>("");
  let testRunning = $state(false);
  async function runTestExpansion() {
    if (!draft) return;
    testRunning = true;
    testOpen = true;
    try {
      testResult = await ipc.expandPromptText(draft.body);
    } catch (e) {
      testResult = `[error: ${fmtErr(e)}]`;
    } finally {
      testRunning = false;
    }
  }

  // ── Import / Export ──
  async function importFile() {
    const picked = await openFileDialog({
      title: "Import .pp.md",
      multiple: false,
      filters: [{ name: "Prompt Player", extensions: ["md"] }],
    });
    if (!picked || Array.isArray(picked)) return;
    try {
      const p = await ipc.importPrompt(picked);
      await refresh();
      selectedId = p.id;
      draft = clone(p);
      dirty = false;
    } catch (e) {
      error = fmtErr(e);
    }
  }
  async function exportFile() {
    if (!draft) return;
    const dest = await saveFileDialog({
      title: "Export prompt",
      defaultPath: `${draft.id}.pp.md`,
      filters: [{ name: "Prompt Player", extensions: ["md"] }],
    });
    if (!dest) return;
    try {
      await ipc.exportPrompt(draft.id, dest);
      flashNotice(`Exported to ${dest}`);
    } catch (e) {
      error = fmtErr(e);
    }
  }

  onMount(() => {
    refresh();
    refreshArmed();
    ipc.libraryRoot().then((r) => (libRoot = r)).catch(() => {});

    // Event-driven, not polled. The backend already knows every mutation
    // (`reindex_after_mutation`) and every arm change (`set_armed_and_report`),
    // and this window is created at launch and stays alive but hidden for the
    // whole session — so a timer here re-serialized the entire prompt library
    // twice a minute, forever, for a window that may never be opened.
    const subs: Promise<UnlistenFn>[] = [
      listen(LIBRARY_CHANGED, () => {
        if (!dirty) refresh();
      }),
      listen<boolean>(ARMED_CHANGED, (e) => {
        armed = e.payload;
      }),
      onWindowShown(() => {
        refreshArmed();
        if (!dirty) refresh();
      }),
    ];

    return () => {
      for (const s of subs) s.then((u) => u()).catch(() => {});
    };
  });
</script>

<svelte:window on:keydown={onKey} on:mousedown={startDrag} />

<div class="app">
  <!-- Single compact ribbon. The whole strip is draggable; buttons
       opt out via the .topbar-actions early-return in startDrag. -->
  <header class="ribbon">
    <div class="brand">
      <span class="chevron">›</span>
      <span class="brand-name">Prompt Player</span>
    </div>
    <div class="topbar-actions">
      <button
        class="ghost"
        class:active={view === "companion"}
        onclick={() => (view = view === "companion" ? "prompts" : "companion")}
        title="Sources, setlist, agent imports and behaviour"
      >
        Companion
      </button>
      <button class="ghost" onclick={createNew} title={`New prompt (${NEW_HINT})`}>
        + New
      </button>
      <button class="ghost" onclick={importFile} title="Import a .pp.md file">
        Import…
      </button>
      <button class="ghost" onclick={exportFile} disabled={!draft} title="Export the selected prompt">
        Export…
      </button>
      <button class="arm-btn" class:armed onclick={toggleArmed} title="Global enable/disable">
        <span class="dot"></span>
        {armed ? "Enabled" : "Disabled"}
      </button>
    </div>
  </header>

  {#if error}
    <div class="banner err">
      {error}
      <button onclick={() => (error = null)}>×</button>
    </div>
  {/if}
  {#if notice}
    <div class="banner ok">
      {notice}
      <button onclick={() => (notice = null)}>×</button>
    </div>
  {/if}

  <div class="layout">
    <aside class="sidebar glass">
      <div class="sidebar-head">
        <span class="label">Prompts</span>
        <span class="count">{filter ? `${visiblePrompts.length}/${prompts.length}` : prompts.length}</span>
      </div>
      <div class="sidebar-filter">
        <input
          class="filter-input"
          type="text"
          placeholder="Filter prompts…"
          bind:value={filter}
          onkeydown={onSidebarKey}
          spellcheck="false"
        />
      </div>
      <ul class="prompt-list">
        {#each visiblePrompts as p (p.id)}
          <li class="row" class:disabled-row={!p.enabled}>
            <button
              class="prompt-item"
              class:active={selectedId === p.id}
              class:dim={!p.enabled}
              title={p.name}
              onclick={() => selectPrompt(p)}
              onkeydown={onSidebarKey}
            >
              <div class="prompt-name">{p.name}</div>
              <div class="prompt-trigs">
                {#each p.triggers as t}
                  <code class="chip">{t}{p.commit_char}</code>
                {/each}
              </div>
            </button>
            <button
              class="row-pin"
              class:on={p.pinned}
              aria-pressed={p.pinned}
              title={p.pinned ? "Unpin from menu bar" : "Pin to menu bar"}
              onclick={(e) => togglePromptPinned(p, e)}
            >
              <!-- Vertical thumbtack — recognizable at small sizes. The on/off
                   states differ in fill (solid blue) vs outline (~60% gray)
                   so it's obvious whether a prompt is pinned without hover. -->
              {#if p.pinned}
                <svg viewBox="0 0 24 24" width="15" height="15" aria-hidden="true">
                  <path fill="currentColor" d="M16 9V4l1-1V2H7v1l1 1v5l-2 2v2h5v7l1 1 1-1v-7h5v-2l-2-2z"/>
                </svg>
              {:else}
                <svg viewBox="0 0 24 24" width="15" height="15" aria-hidden="true">
                  <path fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" stroke-linecap="round" d="M16 9V4l1-1V2H7v1l1 1v5l-2 2v2h5v7l1 1 1-1v-7h5v-2z"/>
                </svg>
              {/if}
            </button>
            <button
              class="row-switch"
              class:on={p.enabled}
              role="switch"
              aria-checked={p.enabled}
              title={p.enabled ? "Disable this prompt" : "Enable this prompt"}
              onclick={(e) => togglePromptEnabled(p, e)}
            >
              <span class="row-knob"></span>
            </button>
          </li>
        {/each}
        {#if prompts.length === 0}
          <li class="empty quick-start">
            <div class="qs-title">Welcome to Prompt Player</div>
            <ol class="qs-steps">
              <li>Click <kbd>+ New</kbd> to create a prompt.</li>
              <li>Pin it (📌) to surface it in the menu bar.</li>
              <li>Type its trigger anywhere — e.g. <kbd>hello&gt;</kbd> — to fire it.</li>
              <li>Press <kbd>{IS_MAC ? "⌘⌥\\" : "Ctrl+Alt+\\"}</kbd> to open the picker.</li>
            </ol>
          </li>
        {:else if visiblePrompts.length === 0}
          <li class="empty">No matches for “{filter}”</li>
        {/if}
      </ul>
      {#if libRoot}
        <button
          class="lib-root"
          onclick={copyLibRoot}
          title={`${libRoot} — click to copy`}
        >
          <span class="lib-root-path">{copiedRoot ? "Copied ✓" : libRoot}</span>
        </button>
      {/if}
    </aside>

    <main class="content">
      {#if view === "companion"}
        <section class="pane glass companion-pane">
          <CompanionPanel
            {prompts}
            onLibraryChanged={refresh}
            onNotice={flashNotice}
            onError={(m) => (error = m)}
          />
        </section>
      {:else if draft}
        <header class="prompt-head">
          {#if draftIsRemote}
            <div class="remote-note">
              From a shared source — read-only, because refreshing the source
              replaces its files. <button class="link" onclick={forkCurrent}>Fork
              into my library</button> to edit it.
            </div>
          {/if}
          <div class="head-row">
            <input
              class="title-input"
              bind:value={draft.name}
              oninput={markDirty}
              placeholder="Prompt name"
              readonly={draftIsRemote}
            />
            <div class="actions">
              <span class="status" class:dirty class:saved={saveStatus === "saved"}>
                {#if saveStatus === "saving"}
                  Saving…
                {:else if saveStatus === "saved" && !dirty}
                  Saved ✓
                {:else if dirty}
                  Unsaved
                {/if}
              </span>
              <button
                class="prompt-toggle"
                class:on={draft.enabled}
                role="switch"
                aria-checked={draft.enabled}
                onclick={toggleDraftEnabled}
                title="Enable / disable this prompt"
              >
                <span class="prompt-knob"></span>
                <span class="prompt-toggle-label">{draft.enabled ? "Enabled" : "Disabled"}</span>
              </button>
              <button
                class="pin-btn"
                class:on={draft.pinned}
                aria-pressed={draft.pinned}
                onclick={toggleDraftPinned}
                title={draft.pinned ? "Unpin from menu bar" : "Pin to menu bar"}
              >
                {#if draft.pinned}
                  <svg viewBox="0 0 24 24" width="13" height="13" aria-hidden="true">
                    <path fill="currentColor" d="M16 9V4l1-1V2H7v1l1 1v5l-2 2v2h5v7l1 1 1-1v-7h5v-2l-2-2z"/>
                  </svg>
                  Pinned
                {:else}
                  <svg viewBox="0 0 24 24" width="13" height="13" aria-hidden="true">
                    <path fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" stroke-linecap="round" d="M16 9V4l1-1V2H7v1l1 1v5l-2 2v2h5v7l1 1 1-1v-7h5v-2z"/>
                  </svg>
                  Pin
                {/if}
              </button>
              <button class="ghost danger" onclick={deleteCurrent}>Delete</button>
              <button
                class="primary"
                onclick={save}
                disabled={!dirty || !!hotkeyError}
                title={hotkeyError ? `Fix the hotkey first — ${hotkeyError}` : undefined}
              >
                Save
              </button>
            </div>
          </div>
          <input
            class="desc-input"
            bind:value={draft.description}
            oninput={markDirty}
            placeholder="One-line description"
          />
          <div class="stats">
            <span>{charCount} chars</span>
            <span>·</span>
            <span>{wordCount} words</span>
            <span>·</span>
            <span>~{estSeconds}s typing at current profile</span>
          </div>
        </header>

        <nav class="tabs glass">
          <button class:active={tab === "edit"} onclick={() => (tab = "edit")}>
            Edit
          </button>
          <button class:active={tab === "preview"} onclick={() => (tab = "preview")}>
            Cadence preview
          </button>
        </nav>

        {#if tab === "edit"}
          <section class="pane glass">
            <div class="form-grid">
              <div class="field">
                <label for="triggers">Triggers</label>
                <input
                  id="triggers"
                  value={listToString(draft.triggers)}
                  oninput={(e) => {
                    if (!draft) return;
                    draft.triggers = stringToList((e.target as HTMLInputElement).value);
                    markDirty();
                  }}
                  placeholder="trigger, alias, alias2"
                />
                <small>Comma-separated. User types one of these + commit char.</small>
              </div>

              <div class="field narrow">
                <label for="commit">Commit char</label>
                <input
                  id="commit"
                  bind:value={draft.commit_char}
                  oninput={markDirty}
                  maxlength="3"
                  placeholder=">"
                />
              </div>

              <div class="field">
                <label for="newline">Line breaks</label>
                <select
                  id="newline"
                  value={draft.newline_mode ?? ""}
                  onchange={(e) => {
                    if (!draft) return;
                    const v = (e.currentTarget as HTMLSelectElement).value;
                    draft.newline_mode = v === "" ? null : (v as NewlineMode);
                    markDirty();
                  }}
                >
                  <option value="">Library default</option>
                  <option value="shift-enter">Shift+Enter (chat apps)</option>
                  <option value="backslash-enter">Backslash + Enter (terminal agents)</option>
                  <option value="plain">Plain Enter</option>
                </select>
                <small>
                  Terminals usually send Shift+Enter as a plain Enter, which
                  submits a prompt at its first blank line.
                </small>
              </div>

              <div class="field">
                <label for="profile">Typing profile</label>
                <select
                  id="profile"
                  bind:value={draft.typing_profile}
                  onchange={markDirty}
                >
                  <option value="sales-engineer">{profileLabel("sales-engineer")}</option>
                  <option value="fast-presenter">{profileLabel("fast-presenter")}</option>
                  <option value="thoughtful-ceo">{profileLabel("thoughtful-ceo")}</option>
                  <option value="custom">{profileLabel("custom")}</option>
                </select>
              </div>

              <div class="field narrow">
                <label for="priority">Priority</label>
                <input
                  id="priority"
                  type="number"
                  bind:value={draft.priority}
                  oninput={markDirty}
                />
                <small>Higher wins on trigger conflicts.</small>
              </div>

              <div class="field">
                <label for="tags">Tags</label>
                <input
                  id="tags"
                  value={listToString(draft.tags ?? [])}
                  oninput={(e) => {
                    if (!draft) return;
                    draft.tags = stringToList((e.target as HTMLInputElement).value);
                    markDirty();
                  }}
                  placeholder="comma, separated"
                />
              </div>

              <div class="field">
                <div class="field-label">Hotkey</div>
                <HotkeyRecorder
                  bind:value={
                    () => draft?.hotkey ?? null,
                    (v) => {
                      if (!draft) return;
                      draft.hotkey = v;
                      markDirty();
                    }
                  }
                  prompts={prompts}
                  selfId={draft.id}
                  onvalidity={(err) => (hotkeyError = err)}
                />
                {#if hotkeyError && dirty}
                  <small class="hint">Save is disabled until the hotkey is fixed or cleared.</small>
                {/if}
              </div>
            </div>

            <details class="overrides">
              <summary>Scope (per-app routing)</summary>
              <div class="form-grid">
                <div class="field" style="grid-column: 1 / -1">
                  <div class="field-label">Apps (bundle ID / executable)</div>
                  <div class="chips">
                    {#each (draft.scope?.app ?? []) as bid (bid)}
                      <span class="app-chip">
                        {bid}
                        <button class="app-chip-x" onclick={() => removeScopeApp(bid)} title="Remove">×</button>
                      </span>
                    {/each}
                    {#if (draft.scope?.app ?? []).length === 0}
                      <span class="chip-empty">No apps yet — capture one or paste a bundle ID below.</span>
                    {/if}
                  </div>
                  <div class="row gap">
                    <input
                      class="grow"
                      placeholder="com.example.app  (or full /path/to/app.exe)"
                      onkeydown={(e) => {
                        if (e.key === "Enter") {
                          const el = e.target as HTMLInputElement;
                          addScopeApp(el.value);
                          el.value = "";
                        }
                      }}
                    />
                    <button class="ghost" onclick={captureScope} disabled={capturing} title="Library hides for 3s while you focus the target app">
                      {capturing ? `Capturing… ${captureCountdown}` : "Capture (3s)"}
                    </button>
                  </div>
                  {#if captureMsg}
                    <small class="hint">{captureMsg}</small>
                  {/if}
                </div>

                <div class="field" style="grid-column: 1 / -1">
                  <label for="wtitle">Window title regex (optional)</label>
                  <input
                    id="wtitle"
                    placeholder=".*chat.*"
                    value={draft.scope?.["window-title-regex"] ?? ""}
                    oninput={(e) => {
                      if (!draft) return;
                      const v = (e.target as HTMLInputElement).value;
                      ensureScope(draft)["window-title-regex"] = v === "" ? null : v;
                      markDirty();
                    }}
                  />
                  <small>Narrows match within the selected apps. Per §4.2.</small>
                </div>
              </div>
            </details>

            <details class="overrides">
              <summary>Cadence overrides (advanced)</summary>
              <div class="form-grid">
                <div class="field narrow">
                  <label for="iki">IKI median (ms)</label>
                  <input
                    id="iki"
                    type="number"
                    min="1"
                    placeholder="140"
                    value={tov(draft)["iki-median-ms"] ?? ""}
                    oninput={(e) => {
                      if (!draft) return;
                      const v = (e.target as HTMLInputElement).value;
                      ensureTov(draft)["iki-median-ms"] =
                        positiveOrNull(v);
                      markDirty();
                    }}
                  />
                  <small>Lower = faster typing. Profile baseline ≈ 140 ms.</small>
                </div>
                <div class="field narrow">
                  <label for="typo">Typo rate</label>
                  <input
                    id="typo"
                    type="number"
                    min="0"
                    max="1"
                    step="0.001"
                    placeholder="0.011"
                    value={tov(draft)["typo-rate"] ?? ""}
                    oninput={(e) => {
                      if (!draft) return;
                      const v = (e.target as HTMLInputElement).value;
                      ensureTov(draft)["typo-rate"] =
                        rateOrNull(v);
                      markDirty();
                    }}
                  />
                  <small>Per-char probability. 0.011 ≈ 1/90 chars.</small>
                </div>
                <div class="field narrow">
                  <label for="pvar">Pause variance</label>
                  <input
                    id="pvar"
                    type="number"
                    min="0.01"
                    step="0.1"
                    placeholder="1.0"
                    value={tov(draft)["pause-variance-scale"] ?? ""}
                    oninput={(e) => {
                      if (!draft) return;
                      const v = (e.target as HTMLInputElement).value;
                      ensureTov(draft)["pause-variance-scale"] =
                        positiveOrNull(v);
                      markDirty();
                    }}
                  />
                  <small>1 = baseline, 1.5 = thoughtful, 0.5 = uniform.</small>
                </div>
                <div class="field check-row">
                  <label>
                    <input
                      type="checkbox"
                      checked={tov(draft)["typos-enabled"] !== false}
                      onchange={(e) => {
                        if (!draft) return;
                        ensureTov(draft)["typos-enabled"] =
                          (e.target as HTMLInputElement).checked;
                        markDirty();
                      }}
                    />
                    Typos enabled
                  </label>
                  <label>
                    <input
                      type="checkbox"
                      checked={tov(draft)["burst-enabled"] !== false}
                      onchange={(e) => {
                        if (!draft) return;
                        ensureTov(draft)["burst-enabled"] =
                          (e.target as HTMLInputElement).checked;
                        markDirty();
                      }}
                    />
                    Burst (muscle-memory) bigrams
                  </label>
                  <label>
                    <input
                      type="checkbox"
                      checked={tov(draft)["pre-submit-pause-enabled"] !== false}
                      onchange={(e) => {
                        if (!draft) return;
                        ensureTov(draft)["pre-submit-pause-enabled"] =
                          (e.target as HTMLInputElement).checked;
                        markDirty();
                      }}
                    />
                    Pre-submit pause
                  </label>
                  <label>
                    <input
                      type="checkbox"
                      checked={!!tov(draft)["send-final-enter"]}
                      onchange={(e) => {
                        if (!draft) return;
                        ensureTov(draft)["send-final-enter"] =
                          (e.target as HTMLInputElement).checked;
                        markDirty();
                      }}
                    />
                    Press Enter at end
                  </label>
                </div>
              </div>
            </details>

            <div class="body-section">
              <div class="body-header">
                <label class="body-label" for="prompt-body">Body</label>
                <div class="body-actions">
                  <button class="ghost small" onclick={runTestExpansion} disabled={testRunning} title="Evaluate placeholders + ${'${{...}}'} expressions against now">
                    {testRunning ? "Testing…" : "Test expansion"}
                  </button>
                  {#if testOpen}
                    <button class="ghost small" onclick={() => (testOpen = false)} title="Hide test result">Hide</button>
                  {/if}
                </div>
              </div>
              <textarea
                id="prompt-body"
                bind:value={draft.body}
                oninput={markDirty}
                spellcheck="false"
                placeholder="The text the app will type..."
              ></textarea>
              <small class="body-hint">
                Placeholders, built-in variables and TypeScript expressions.
                <button class="linky" onclick={() => (refOpen = !refOpen)}>
                  {refOpen ? "Hide reference" : "Show reference"}
                </button>
              </small>
              {#if refOpen}
                <div class="ref-pane">
                  {#each SNIPPET_GROUPS as group}
                    <div class="ref-group">
                      <div class="ref-title">{group.title}</div>
                      <div class="ref-chips">
                        {#each group.items as item}
                          <button
                            class="chip snippet"
                            title={item.help}
                            onclick={() => insertSnippet(item.insert)}
                          >
                            <code>{item.label}</code>
                          </button>
                        {/each}
                      </div>
                    </div>
                  {/each}
                  <small class="hint">Click to insert at the cursor.</small>
                </div>
              {/if}
              {#if testOpen}
                <div class="test-pane">
                  <div class="test-header">
                    <span>Expanded preview</span>
                    <small class="hint">No clipboard / selection / app context — those resolve at fire time.</small>
                  </div>
                  <pre class="test-body">{testResult || "(empty)"}</pre>
                </div>
              {/if}
            </div>
          </section>
        {:else}
          <section class="pane glass">
            <CadencePreview
              body={draft.body}
              profile={draft.typing_profile}
              overrides={draft.typing_overrides ?? null}
            />
          </section>
        {/if}
      {:else}
        <div class="empty-state">
          <h2>No prompt selected</h2>
          <p>Pick one from the list, or click <strong>+ New</strong> to create one.</p>
        </div>
      {/if}
    </main>
  </div>
</div>

<style>
  /* Apple system color palette (UIColor.systemBackground et al, elevated tier).
     Light mode mirrors Finder's white-pane / gray-sidebar; dark mode uses
     Apple's true system grays (#1c1c1e / #2c2c2e / #3a3a3c). */
  :global(:root) {
    --bg-window:        #f2f2f7;             /* secondarySystemBackground light */
    --bg-content:       #ffffff;             /* systemBackground light */
    --bg-sidebar:       #f2f2f7;             /* sidebar = window */
    --bg-input:         #ffffff;
    --bg-input-focus:   #ffffff;
    --bg-card:          #fafafa;
    --hover:            rgba(60, 60, 67, 0.06);
    --selection:        rgba(0, 122, 255, 0.18);
    --selection-border: rgba(0, 122, 255, 0.45);
    --accent:           #007aff;
    --accent-fg:        #ffffff;
    --text:             rgba(0, 0, 0, 0.92);
    --text-secondary:   rgba(60, 60, 67, 0.6);
    --text-muted:       rgba(60, 60, 67, 0.42);
    --border:           rgba(60, 60, 67, 0.18);
    --border-strong:    rgba(60, 60, 67, 0.3);
    --separator:        rgba(60, 60, 67, 0.12);
    --danger:           #ff3b30;
    --danger-bg:        rgba(255, 59, 48, 0.1);
    --success:          #34c759;
    --warning:          #ff9500;
  }
  @media (prefers-color-scheme: dark) {
    :global(:root) {
      --bg-window:        #1c1c1e;           /* secondarySystemBackground dark */
      --bg-content:       #2c2c2e;           /* tertiarySystemBackground (elevated) */
      --bg-sidebar:       #1c1c1e;
      --bg-input:         #1c1c1e;
      --bg-input-focus:   #2c2c2e;
      --bg-card:          #2c2c2e;
      --hover:            rgba(235, 235, 245, 0.06);
      --selection:        rgba(10, 132, 255, 0.32);
      --selection-border: rgba(10, 132, 255, 0.55);
      --accent:           #0a84ff;
      --accent-fg:        #ffffff;
      --text:             rgba(255, 255, 255, 0.92);
      --text-secondary:   rgba(235, 235, 245, 0.6);
      --text-muted:       rgba(235, 235, 245, 0.3);
      --border:           rgba(84, 84, 88, 0.65);
      --border-strong:    rgba(120, 120, 128, 0.55);
      --separator:        rgba(84, 84, 88, 0.45);
      --danger:           #ff453a;
      --danger-bg:        rgba(255, 69, 58, 0.18);
      --success:          #32d74b;
      --warning:          #ff9f0a;
    }
  }

  :global(html), :global(body), :global(#app) {
    margin: 0;
    height: 100%;
    color: var(--text);
    font-family: -apple-system, BlinkMacSystemFont, "SF Pro", "SF Pro Text",
                 "Helvetica Neue", sans-serif;
    font-size: 13px;
    -webkit-font-smoothing: antialiased;
    /* Was previously `transparent` on html/body + `border-radius: 12px` on
       #app — that left the rounded-rect's corners showing the desktop
       behind the window (transparent flag in tauri.conf.json paired with
       a CSS-rounded content layer). macOS already provides a window-level
       rounded mask + shadow for plain (non-transparent) NSWindows; let it
       handle that and paint the content edge-to-edge here. */
    background: var(--bg-window);
  }
  :global(#app) {
    background: var(--bg-window);
  }
  :global(*) { box-sizing: border-box; }

  .app { display: flex; flex-direction: column; height: 100vh; }

  /* Finder-style elevated panel */
  .glass {
    background: var(--bg-content);
    border: 1px solid var(--border);
  }

  /* Compact ribbon: 40px tall, traffic lights overlay the leftmost ~78px. */
  .ribbon {
    display: flex;
    justify-content: space-between;
    align-items: center;
    height: 40px;
    flex-shrink: 0;
    padding: 0 12px 0 88px;
    border-bottom: 1px solid var(--separator);
    cursor: default;
  }
  .ribbon:active { cursor: grabbing; }
  .brand { display: flex; align-items: center; gap: 6px; }
  .chevron {
    font-family: ui-monospace, "SF Mono", monospace;
    font-size: 16px;
    font-weight: 600;
    color: var(--accent);
  }
  .brand-name { font-weight: 600; font-size: 13px; color: var(--text); }
  .topbar-actions {
    display: flex;
    gap: 8px;
    align-items: center;
    -webkit-app-region: no-drag;
  }

  /* Finder/AppKit-style buttons */
  button {
    cursor: pointer;
    font-family: inherit;
    font-size: 12px;
    font-weight: 500;
    padding: 4px 11px;
    border-radius: 5px;
    border: 1px solid var(--border);
    background: var(--bg-content);
    color: var(--text);
    transition: background 0.1s, border-color 0.1s;
  }
  button:hover { background: var(--hover); border-color: var(--border-strong); }
  button:disabled { opacity: 0.4; cursor: not-allowed; }
  button.ghost {
    background: transparent;
    border-color: transparent;
  }
  button.ghost:hover {
    background: var(--hover);
    border-color: var(--border);
  }
  button.ghost.danger {
    color: var(--danger);
    background: transparent;
    border-color: transparent;
  }
  button.ghost.danger:hover {
    background: var(--danger-bg);
    border-color: rgba(255, 69, 58, 0.35);
  }
  button.primary {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--accent-fg);
    font-weight: 500;
  }
  button.primary:hover:not(:disabled) { filter: brightness(1.08); }
  button.primary:disabled { background: var(--accent); opacity: 0.4; }

  /* Arm pill — Finder-style status indicator */
  .arm-btn {
    display: flex;
    align-items: center;
    gap: 6px;
    border-radius: 999px;
    padding: 4px 11px;
  }
  .arm-btn .dot {
    width: 7px; height: 7px; border-radius: 50%;
    background: var(--text-muted);
  }
  .arm-btn.armed {
    color: var(--success);
    background: rgba(52, 199, 89, 0.12);
    border-color: rgba(52, 199, 89, 0.3);
  }
  .arm-btn.armed .dot {
    background: var(--success);
    box-shadow: 0 0 0 2px rgba(52, 199, 89, 0.2);
  }

  /* Finder-style two-pane: sidebar with its own bg + main content area */
  .layout {
    display: grid;
    grid-template-columns: 240px 1fr;
    flex: 1;
    overflow: hidden;
  }
  .sidebar {
    background: var(--bg-sidebar);
    border-right: 1px solid var(--separator);
    display: flex;
    flex-direction: column;
    overflow: hidden;
    border-radius: 0;
    border-top: none;
    border-left: none;
    border-bottom: none;
  }
  .sidebar-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 14px 14px 6px;
  }
  .label {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-secondary);
    font-weight: 600;
  }
  .count {
    font-size: 10px;
    color: var(--text-muted);
    background: var(--hover);
    padding: 1px 7px;
    border-radius: 10px;
  }
  /* Sidebar filter — compact text field above the list. */
  .sidebar-filter { padding: 2px 10px 6px; }
  .filter-input {
    width: 100%;
    padding: 4px 8px;
    font-size: 12px;
    font-family: inherit;
    color: var(--text);
    background: var(--bg-input);
    border: 1px solid var(--border);
    border-radius: 5px;
  }
  .filter-input::placeholder { color: var(--text-muted); }
  .filter-input:focus {
    border-color: var(--selection-border);
    outline: none;
  }

  .prompt-list {
    list-style: none; margin: 0; padding: 2px 6px 8px;
    overflow-y: auto; flex: 1;
  }
  .prompt-list li.empty {
    padding: 16px; color: var(--text-muted);
    font-size: 12px; line-height: 1.5; text-align: center;
  }

  /* Library folder path — sidebar footer, click-to-copy. */
  .lib-root {
    display: block;
    width: 100%;
    padding: 6px 14px;
    background: transparent;
    border: none;
    border-top: 1px solid var(--separator);
    border-radius: 0;
    text-align: left;
    cursor: pointer;
    flex-shrink: 0;
  }
  .lib-root:hover { background: var(--hover); border-color: var(--separator); }
  .lib-root-path {
    display: block;
    font-family: ui-monospace, "SF Mono", Menlo, monospace;
    font-size: 10px;
    color: var(--text-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    direction: rtl; /* Truncate from the left so the folder name stays visible. */
    text-align: left;
  }
  .lib-root:hover .lib-root-path { color: var(--text-secondary); }
  kbd {
    background: var(--hover);
    padding: 1px 5px;
    border-radius: 3px;
    font-family: ui-monospace, "SF Mono", monospace;
    font-size: 11px;
  }
  .prompt-list li.row {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0;
    margin: 0;
  }
  .prompt-list li.row.disabled-row .prompt-name,
  .prompt-list li.row.disabled-row .prompt-trigs { opacity: 0.5; }

  .prompt-item {
    flex: 1;
    display: block; text-align: left;
    background: transparent; border: 1px solid transparent;
    padding: 7px 10px;
    border-radius: 6px; cursor: pointer;
    transition: background 0.08s; color: inherit;
    min-width: 0;
  }
  .prompt-item.dim { opacity: 0.65; }
  .prompt-item:hover { background: var(--hover); }
  .prompt-item.active {
    background: var(--selection);
    border-color: var(--selection-border);
    opacity: 1;
  }

  /* Per-row enable/disable switch (small, sits to the right of the row). */
  .row-switch {
    position: relative;
    width: 26px; height: 14px;
    border-radius: 999px;
    background: rgba(120, 120, 128, 0.32);
    border: none;
    padding: 0;
    cursor: pointer;
    flex-shrink: 0;
    margin-right: 8px;
    transition: background-color 120ms ease;
  }
  .row-switch.on { background: rgba(48, 209, 88, 1); }
  .row-knob {
    position: absolute;
    top: 1px; left: 1px;
    width: 12px; height: 12px;
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 0.5px 1px rgba(0,0,0,0.2), 0 1px 2px rgba(0,0,0,0.18);
    transition: transform 140ms ease;
  }
  .row-switch.on .row-knob { transform: translateX(12px); }

  /* Pin button on each list row. Always visible — outline at ~65% opacity
     when not pinned, filled accent blue when pinned. Without the always-on
     visibility, users can't discover it (real feedback from a v0.1 review).
     Slight bg lift on hover for click affordance. */
  .row-pin {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 26px; height: 26px;
    margin-right: 2px;
    background: transparent;
    border: none;
    border-radius: 5px;
    color: rgba(255, 255, 255, 0.55);
    cursor: pointer;
    flex-shrink: 0;
    transition: color 120ms ease, background-color 120ms ease;
  }
  .row-pin:hover { background: rgba(255, 255, 255, 0.10); color: rgba(255, 255, 255, 0.92); }
  .row-pin.on { color: rgba(10, 132, 255, 1); background: rgba(10, 132, 255, 0.10); }
  .row-pin.on:hover { color: rgba(10, 132, 255, 1); background: rgba(10, 132, 255, 0.18); }

  /* Pin button in the editor header — text + icon, sits next to the
     Enabled toggle. */
  .pin-btn {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 4px 8px;
    border: 1px solid var(--separator);
    border-radius: 5px;
    background: transparent;
    color: var(--text-secondary, rgba(255,255,255,0.6));
    font: inherit;
    font-size: 12px;
    cursor: pointer;
    transition: color 120ms ease, background-color 120ms ease, border-color 120ms ease;
  }
  .pin-btn:hover {
    background: rgba(255, 255, 255, 0.06);
    color: var(--text, rgba(255,255,255,0.92));
  }
  .pin-btn.on {
    color: rgba(10, 132, 255, 1);
    border-color: rgba(10, 132, 255, 0.4);
    background: rgba(10, 132, 255, 0.10);
  }
  .pin-btn.on:hover {
    background: rgba(10, 132, 255, 0.18);
  }
  @media (prefers-color-scheme: light) {
    .row-pin { color: rgba(0, 0, 0, 0.5); }
    .row-pin:hover { background: rgba(0, 0, 0, 0.07); color: rgba(0, 0, 0, 0.88); }
    .row-pin.on { color: rgba(0, 122, 255, 1); background: rgba(0, 122, 255, 0.08); }
    .row-pin.on:hover { background: rgba(0, 122, 255, 0.14); }
    .pin-btn { color: rgba(0,0,0,0.6); }
    .pin-btn:hover { background: rgba(0,0,0,0.05); color: rgba(0,0,0,0.88); }
    .pin-btn.on {
      color: rgba(0, 122, 255, 1);
      border-color: rgba(0, 122, 255, 0.4);
      background: rgba(0, 122, 255, 0.08);
    }
  }

  /* Quick Start panel — replaces the bare "No prompts" empty state. */
  .quick-start {
    padding: 16px 14px !important;
    text-align: left !important;
    color: var(--text-secondary, rgba(255,255,255,0.7)) !important;
  }
  .qs-title {
    font-size: 13px;
    font-weight: 600;
    margin-bottom: 8px;
    color: var(--text, rgba(255,255,255,0.92));
  }
  .qs-steps {
    margin: 0;
    padding-left: 18px;
    font-size: 12px;
    line-height: 1.6;
  }
  .qs-steps li { margin-bottom: 2px; }
  .qs-steps kbd {
    font-family: "SF Mono", ui-monospace, Menlo, monospace;
    font-size: 11px;
    background: rgba(255, 255, 255, 0.08);
    border-radius: 3px;
    padding: 1px 5px;
  }
  @media (prefers-color-scheme: light) {
    .qs-steps kbd { background: rgba(0, 0, 0, 0.06); }
  }

  .prompt-name {
    font-weight: 500; font-size: 12.5px;
    margin-bottom: 3px; line-height: 1.3;
    /* Single-line truncation. Full name is available via the parent
       button's title attribute. Stops "Product spec from one-liner" from
       wrapping into two lines and doubling row height. */
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .prompt-trigs { display: flex; gap: 3px; flex-wrap: wrap; }
  .chip {
    font-family: ui-monospace, "SF Mono", monospace;
    font-size: 10px;
    color: var(--accent);
    background: rgba(10, 132, 255, 0.12);
    padding: 1px 5px;
    border-radius: 3px;
    font-weight: 500;
  }
  @media (prefers-color-scheme: light) {
    .chip { background: rgba(0, 122, 255, 0.1); }
  }

  /* Placeholder / expression reference */
  .linky {
    font: inherit;
    background: none;
    border: 0;
    padding: 0;
    color: var(--accent);
    cursor: default;
    text-decoration: underline;
  }
  .ref-pane {
    margin-top: 8px;
    padding: 10px 12px;
    border: 1px solid var(--separator);
    border-radius: 8px;
    background: var(--bg-card);
  }
  .ref-group + .ref-group {
    margin-top: 10px;
  }
  .ref-title {
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    opacity: 0.55;
    margin-bottom: 5px;
  }
  .ref-chips {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
  }
  button.chip.snippet {
    border: 1px solid transparent;
    cursor: default;
  }
  button.chip.snippet:hover {
    border-color: var(--accent);
  }

  /* Content */
  .content {
    overflow-y: auto;
    padding: 18px 24px 24px;
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .prompt-head {
    margin-bottom: 14px;
    padding-bottom: 12px;
    border-bottom: 1px solid var(--separator);
  }
  .head-row {
    display: flex; gap: 12px; align-items: center;
    margin-bottom: 4px;
  }
  .actions { display: flex; gap: 8px; align-items: center; }

  /* Per-prompt toggle pill in the editor header. Same shape as the global
     "Enabled/Disabled" pill in the ribbon for visual consistency. */
  .prompt-toggle {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 3px 10px 3px 4px;
    background: rgba(120, 120, 128, 0.18);
    border: 1px solid var(--border);
    border-radius: 999px;
    color: var(--text-secondary);
    font-size: 12px;
    cursor: pointer;
    transition: background 0.12s, color 0.12s, border-color 0.12s;
  }
  .prompt-toggle .prompt-knob {
    position: relative;
    width: 22px; height: 12px;
    border-radius: 999px;
    background: rgba(120, 120, 128, 0.42);
    transition: background 120ms ease;
  }
  .prompt-toggle .prompt-knob::after {
    content: "";
    position: absolute;
    top: 1px; left: 1px;
    width: 10px; height: 10px;
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 0.5px 1px rgba(0,0,0,0.2);
    transition: transform 140ms ease;
  }
  .prompt-toggle.on { color: var(--text); border-color: rgba(48, 209, 88, 0.45); }
  .prompt-toggle.on .prompt-knob { background: rgba(48, 209, 88, 1); }
  .prompt-toggle.on .prompt-knob::after { transform: translateX(10px); }

  /* Inputs — Finder/AppKit text field style */
  .title-input, .desc-input, .form-grid input, .form-grid select, textarea {
    background: var(--bg-input);
    border: 1px solid var(--border);
    border-radius: 5px;
    color: var(--text);
    font-family: inherit;
    transition: border-color 0.1s, background 0.1s;
  }
  .title-input {
    flex: 1;
    font-size: 18px;
    font-weight: 600;
    padding: 4px 8px;
    background: transparent;
    border-color: transparent;
    margin-left: -8px;
    letter-spacing: -0.01em;
  }
  .title-input:hover { background: var(--hover); }
  .title-input:focus {
    background: var(--bg-input-focus);
    border-color: var(--selection-border);
    outline: none;
  }
  .desc-input {
    width: 100%; padding: 3px 8px; font-size: 12px;
    background: transparent; border-color: transparent;
    color: var(--text-secondary); margin-left: -8px;
    margin-bottom: 8px;
  }
  .desc-input:hover { background: var(--hover); }
  .desc-input:focus {
    background: var(--bg-input-focus);
    border-color: var(--selection-border);
    color: var(--text);
    outline: none;
  }

  .stats {
    color: var(--text-muted);
    font-size: 11px;
    display: flex;
    gap: 6px;
    padding-left: 1px;
  }
  .status {
    font-size: 11px;
    color: var(--text-muted);
    font-weight: 500;
  }
  .status.dirty { color: #ff9500; }
  .status.saved { color: var(--success); }

  /* Tabs — segmented control */
  .tabs {
    display: flex;
    gap: 2px;
    padding: 2px;
    border-radius: 6px;
    margin-bottom: 12px;
    width: fit-content;
    background: var(--hover);
    border: 1px solid var(--separator);
  }
  .tabs button {
    background: transparent;
    border: none;
    padding: 4px 12px;
    cursor: pointer;
    font-size: 12px;
    border-radius: 4px;
    font-weight: 500;
    color: var(--text-secondary);
  }
  .tabs button:hover { color: var(--text); }
  .tabs button.active {
    background: var(--bg-content);
    color: var(--text);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.15);
  }

  /* Pane */
  .pane {
    border-radius: 8px;
    padding: 18px;
    background: var(--bg-content);
    border: 1px solid var(--border);
  }

  /* Form grid */
  .form-grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(180px, 1fr));
    gap: 14px 18px;
    margin-bottom: 14px;
  }
  .field { display: flex; flex-direction: column; gap: 4px; }
  .field.narrow { max-width: 200px; }
  .field label,
  .field-label {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-secondary);
    font-weight: 600;
  }
  .field input, .field select {
    padding: 5px 9px;
    font-size: 13px;
  }
  .field input:focus, .field select:focus {
    border-color: var(--selection-border);
    box-shadow: 0 0 0 3px rgba(10, 132, 255, 0.2);
    outline: none;
  }
  .field small { color: var(--text-muted); font-size: 11px; line-height: 1.4; }
  .check-row {
    grid-column: 1 / -1;
    flex-direction: row !important;
    flex-wrap: wrap;
    gap: 14px !important;
  }
  .check-row label {
    display: flex;
    align-items: center;
    gap: 6px;
    text-transform: none !important;
    letter-spacing: 0 !important;
    font-size: 12px !important;
    font-weight: 500 !important;
    color: inherit !important;
  }

  details.overrides {
    border-top: 1px solid var(--separator);
    padding-top: 12px;
    margin: 8px 0 16px;
  }
  details.overrides summary {
    font-size: 12px;
    font-weight: 600;
    color: var(--text-secondary);
    cursor: pointer;
    padding: 4px 0;
    user-select: none;
  }
  details.overrides[open] summary { margin-bottom: 12px; }

  /* Body section */
  .body-section { margin-top: 4px; }
  .body-label {
    display: block;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-secondary);
    font-weight: 600;
    margin-bottom: 6px;
  }
  textarea {
    width: 100%; min-height: 280px;
    padding: 12px 14px;
    font-family: ui-monospace, "SF Mono", Menlo, monospace;
    font-size: 12.5px; line-height: 1.6;
    resize: vertical;
  }
  textarea:focus {
    border-color: var(--selection-border);
    box-shadow: 0 0 0 3px rgba(10, 132, 255, 0.2);
    outline: none;
  }
  .body-hint {
    display: block;
    margin-top: 8px;
    color: var(--text-muted);
    font-size: 11px;
    line-height: 1.5;
  }

  /* Body header — label + Test/Hide buttons on the right. */
  .body-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 6px;
  }
  .body-header .body-label { margin-bottom: 0; }
  .body-actions { display: flex; gap: 6px; }
  .ghost.small { font-size: 11px; padding: 4px 9px; }

  /* Expansion preview — boxed result of running expressions + placeholders
     against the body. Read-only; <pre> preserves whitespace. */
  .test-pane {
    margin-top: 10px;
    padding: 10px 12px;
    border: 1px solid var(--selection-border);
    border-radius: 6px;
    background: var(--hover);
  }
  .test-header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 6px;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-secondary);
  }
  .test-header .hint {
    text-transform: none;
    letter-spacing: 0;
    color: var(--text-muted);
    font-size: 11px;
  }
  .test-body {
    margin: 0;
    padding: 8px 10px;
    font-family: ui-monospace, "SF Mono", Menlo, monospace;
    font-size: 12px;
    line-height: 1.5;
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 200px;
    overflow: auto;
    background: var(--bg-base, transparent);
    border-radius: 4px;
  }

  /* Scope app chips. Named .app-chip (not .chip) so they don't collide with
     the compact trigger chips in the sidebar list. */
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-bottom: 8px;
    min-height: 24px;
  }
  .app-chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 3px 6px 3px 8px;
    background: var(--hover);
    border: 1px solid var(--selection-border);
    border-radius: 4px;
    font-size: 11.5px;
    font-family: ui-monospace, "SF Mono", Menlo, monospace;
  }
  .app-chip-x {
    background: transparent;
    border: 0;
    color: var(--text-muted);
    cursor: pointer;
    padding: 0 2px;
    font-size: 14px;
    line-height: 1;
  }
  .app-chip-x:hover { color: var(--text); }
  .chip-empty {
    color: var(--text-muted);
    font-size: 11.5px;
    font-style: italic;
  }
  .row.gap { display: flex; gap: 6px; align-items: center; }
  .grow { flex: 1; }
  small.hint {
    display: block;
    margin-top: 4px;
    color: var(--text-muted);
    font-size: 11px;
  }

  /* Empty state */
  .empty-state {
    text-align: center;
    padding: 80px 20px;
    color: var(--text-muted);
  }
  .empty-state h2 {
    margin: 0 0 6px 0;
    font-size: 18px;
    color: var(--text-secondary);
    font-weight: 500;
  }

  /* Banner */
  .banner.err {
    background: var(--danger-bg);
    color: var(--danger);
    padding: 10px 16px;
    border-radius: 8px;
    margin: 0 12px 12px;
    font-size: 13px;
    border: 1px solid rgba(220, 53, 69, 0.2);
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  .banner.err button {
    background: none; border: none; color: inherit;
    font-size: 18px; cursor: pointer;
  }
  /* Success variant — same shape, green palette. */
  .banner.ok {
    background: rgba(52, 199, 89, 0.12);
    color: var(--success);
    padding: 10px 16px;
    border-radius: 8px;
    margin: 0 12px 12px;
    font-size: 13px;
    border: 1px solid rgba(52, 199, 89, 0.3);
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  .banner.ok button {
    background: none; border: none; color: inherit;
    font-size: 18px; cursor: pointer;
  }

  /* Read-only notice on a prompt owned by a remote source. */
  .remote-note {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    align-items: baseline;
    margin: 0 0 8px;
    padding: 6px 10px;
    border-radius: 6px;
    font-size: 12px;
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    color: var(--text-primary);
  }
  .remote-note .link {
    background: none;
    border: none;
    padding: 0;
    font: inherit;
    color: var(--accent);
    text-decoration: underline;
    cursor: pointer;
  }
  .companion-pane {
    padding: 16px 18px;
    overflow-y: auto;
  }
  .topbar-actions .ghost.active {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    color: var(--text-primary);
  }
</style>
