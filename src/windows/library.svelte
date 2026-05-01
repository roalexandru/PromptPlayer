<script lang="ts">
  import { onMount } from "svelte";
  import { ipc, type Prompt, type ProfileKind } from "$lib/ipc";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import CadencePreview from "$lib/components/CadencePreview.svelte";
  import HotkeyRecorder from "$lib/components/HotkeyRecorder.svelte";

  let prompts: Prompt[] = $state([]);
  let selectedId = $state<string | null>(null);
  let armed = $state(false);
  let error = $state<string | null>(null);
  let tab: "edit" | "preview" = $state("edit");

  // Local working copy of the selected prompt (the one we're editing).
  let draft = $state<Prompt | null>(null);
  let dirty = $state(false);
  let saveStatus = $state<"" | "saving" | "saved" | "error">("");

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
  let estSeconds = $derived(
    draft
      ? Math.round((wordCount / profileWpm(draft.typing_profile)) * 60 + 1.0)
      : 0,
  );

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
      error = String(e);
    }
  }

  function clone(p: Prompt): Prompt {
    return JSON.parse(JSON.stringify(p));
  }

  async function refreshArmed() {
    try { armed = await ipc.getArmed(); } catch {}
  }
  async function toggleArmed() { armed = await ipc.toggleArmed(); }

  function selectPrompt(p: Prompt) {
    if (dirty && !confirm("Discard unsaved changes?")) return;
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
    if (!draft) return;
    saveStatus = "saving";
    try {
      await ipc.savePrompt(draft);
      saveStatus = "saved";
      dirty = false;
      await refresh();
    } catch (e) {
      error = String(e);
      saveStatus = "error";
    }
  }

  async function createNew() {
    if (dirty && !confirm("Discard unsaved changes?")) return;
    try {
      const p = await ipc.createPrompt();
      await refresh();
      selectedId = p.id;
      draft = clone(p);
      dirty = false;
    } catch (e) {
      error = String(e);
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
      await ipc.setPromptEnabled(p.id, next);
    } catch (err) {
      error = String(err);
      // Roll back.
      prompts = prompts.map((x) => (x.id === p.id ? { ...x, enabled: !next } : x));
      if (draft && draft.id === p.id) draft = { ...draft, enabled: !next };
    }
  }

  async function toggleDraftEnabled() {
    if (!draft) return;
    await togglePromptEnabled(draft, new Event("synth"));
  }

  async function deleteCurrent() {
    if (!draft) return;
    if (!confirm(`Delete "${draft.name}"? This removes the .pp.md file.`)) return;
    try {
      await ipc.deletePrompt(draft.id);
      selectedId = null;
      draft = null;
      dirty = false;
      await refresh();
    } catch (e) {
      error = String(e);
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

  // Manual drag — call Tauri's startDragging() on mousedown. This works
  // reliably across the transparent + Overlay + macOSPrivateApi config
  // where `data-tauri-drag-region` and `-webkit-app-region: drag` are flaky.
  async function startDrag(e: MouseEvent) {
    const target = e.target as HTMLElement;
    if (target.closest("button, input, select, textarea, .topbar-actions, .arm-btn"))
      return;
    e.preventDefault();
    try {
      await getCurrentWindow().startDragging();
    } catch (err) {
      console.error("startDragging failed:", err);
    }
  }

  // Cmd/Ctrl+S save.
  function onKey(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key === "s") {
      e.preventDefault();
      if (dirty) save();
    }
  }

  onMount(() => {
    refresh();
    refreshArmed();
    const t = setInterval(() => {
      refreshArmed();
      if (!dirty) refresh();
    }, 2000);
    return () => clearInterval(t);
  });
</script>

<svelte:window on:keydown={onKey} />

<div class="app">
  <!-- Single compact ribbon. The whole strip is draggable; buttons
       opt out via the .topbar-actions early-return in startDrag. -->
  <header class="ribbon" onmousedown={startDrag}>
    <div class="brand">
      <span class="chevron">›</span>
      <span class="brand-name">Prompt Player</span>
    </div>
    <div class="topbar-actions">
      <button class="ghost" onclick={createNew} title="New prompt (⌘N)">
        + New
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

  <div class="layout">
    <aside class="sidebar glass">
      <div class="sidebar-head">
        <span class="label">Prompts</span>
        <span class="count">{prompts.length}</span>
      </div>
      <ul class="prompt-list">
        {#each prompts as p (p.id)}
          <li class="row" class:disabled-row={!p.enabled}>
            <button
              class="prompt-item"
              class:active={selectedId === p.id}
              class:dim={!p.enabled}
              onclick={() => selectPrompt(p)}
            >
              <div class="prompt-name">{p.name}</div>
              <div class="prompt-trigs">
                {#each p.triggers as t}
                  <code class="chip">{t}{p.commit_char}</code>
                {/each}
              </div>
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
          <li class="empty">
            No prompts. Click <kbd>+ New</kbd> to create one.
          </li>
        {/if}
      </ul>
    </aside>

    <main class="content">
      {#if draft}
        <header class="prompt-head">
          <div class="head-row">
            <input
              class="title-input"
              bind:value={draft.name}
              oninput={markDirty}
              placeholder="Prompt name"
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
              <button class="ghost danger" onclick={deleteCurrent}>Delete</button>
              <button class="primary" onclick={save} disabled={!dirty}>
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
                  value={listToString(draft.tags)}
                  oninput={(e) => {
                    if (!draft) return;
                    draft.tags = stringToList((e.target as HTMLInputElement).value);
                    markDirty();
                  }}
                  placeholder="comma, separated"
                />
              </div>

              <div class="field">
                <label>Hotkey</label>
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
                />
              </div>
            </div>

            <details class="overrides">
              <summary>Cadence overrides (advanced)</summary>
              <div class="form-grid">
                <div class="field narrow">
                  <label for="iki">IKI median (ms)</label>
                  <input
                    id="iki"
                    type="number"
                    placeholder="140"
                    value={draft.typing_overrides["iki-median-ms"] ?? ""}
                    oninput={(e) => {
                      if (!draft) return;
                      const v = (e.target as HTMLInputElement).value;
                      draft.typing_overrides["iki-median-ms"] =
                        v === "" ? null : Number(v);
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
                    step="0.001"
                    placeholder="0.011"
                    value={draft.typing_overrides["typo-rate"] ?? ""}
                    oninput={(e) => {
                      if (!draft) return;
                      const v = (e.target as HTMLInputElement).value;
                      draft.typing_overrides["typo-rate"] =
                        v === "" ? null : Number(v);
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
                    step="0.1"
                    placeholder="1.0"
                    value={draft.typing_overrides["pause-variance-scale"] ?? ""}
                    oninput={(e) => {
                      if (!draft) return;
                      const v = (e.target as HTMLInputElement).value;
                      draft.typing_overrides["pause-variance-scale"] =
                        v === "" ? null : Number(v);
                      markDirty();
                    }}
                  />
                  <small>1 = baseline, 1.5 = thoughtful, 0.5 = uniform.</small>
                </div>
                <div class="field check-row">
                  <label>
                    <input
                      type="checkbox"
                      checked={draft.typing_overrides["typos-enabled"] !== false}
                      onchange={(e) => {
                        if (!draft) return;
                        draft.typing_overrides["typos-enabled"] =
                          (e.target as HTMLInputElement).checked;
                        markDirty();
                      }}
                    />
                    Typos enabled
                  </label>
                  <label>
                    <input
                      type="checkbox"
                      checked={draft.typing_overrides["burst-enabled"] !== false}
                      onchange={(e) => {
                        if (!draft) return;
                        draft.typing_overrides["burst-enabled"] =
                          (e.target as HTMLInputElement).checked;
                        markDirty();
                      }}
                    />
                    Burst (muscle-memory) bigrams
                  </label>
                  <label>
                    <input
                      type="checkbox"
                      checked={draft.typing_overrides["pre-submit-pause-enabled"] !== false}
                      onchange={(e) => {
                        if (!draft) return;
                        draft.typing_overrides["pre-submit-pause-enabled"] =
                          (e.target as HTMLInputElement).checked;
                        markDirty();
                      }}
                    />
                    Pre-submit pause
                  </label>
                  <label>
                    <input
                      type="checkbox"
                      checked={!!draft.typing_overrides["send-final-enter"]}
                      onchange={(e) => {
                        if (!draft) return;
                        draft.typing_overrides["send-final-enter"] =
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
              <label class="body-label">Body</label>
              <textarea
                bind:value={draft.body}
                oninput={markDirty}
                spellcheck="false"
                placeholder="The text the app will type..."
              ></textarea>
              <small class="body-hint">
                Supports VS Code-style placeholders (<code>$1</code>, <code>$0</code>,
                <code>${"{1|a,b|}"}</code>), built-in vars
                (<code>$CLIPBOARD</code>, <code>$SELECTION</code>, <code>$DATE</code>…),
                and TypeScript expressions (<code>${"{{ now.toISOString() }}"}</code>).
              </small>
            </div>
          </section>
        {:else}
          <section class="pane glass">
            <CadencePreview body={draft.body} profile={draft.typing_profile} />
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
  }
  :global(html), :global(body) { background: transparent; }
  :global(#app) {
    background: var(--bg-window);
    border-radius: 12px;
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
  .prompt-list {
    list-style: none; margin: 0; padding: 2px 6px 8px;
    overflow-y: auto; flex: 1;
  }
  .prompt-list li.empty {
    padding: 16px; color: var(--text-muted);
    font-size: 12px; line-height: 1.5; text-align: center;
  }
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
  .prompt-name {
    font-weight: 500; font-size: 13px;
    margin-bottom: 3px; line-height: 1.3;
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
  .field label {
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
  .body-hint code {
    background: var(--hover);
    padding: 1px 5px;
    border-radius: 3px;
    font-size: 11px;
    color: var(--text-secondary);
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
</style>
