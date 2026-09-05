<!--
  Companion settings for the library window: remote prompt sources, the demo
  setlist, agent-prompt import, and the handful of `promptplayer.yaml` keys
  worth a UI.

  §10.3 is explicit that there is no Settings *window* — cross-cutting config
  lives in one YAML file the user can edit directly. This panel writes that
  same file rather than introducing a second source of truth, and says so, so
  a hand-edit and an in-app edit never disagree.
-->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { open as openDirDialog } from "@tauri-apps/plugin-dialog";
  import {
    ipc,
    fmtErr,
    type AppConfig,
    type SourceStatus,
    type SetlistEntry,
    type Prompt,
    type NewlineMode,
    type PickerDisplay,
    type PendingChange,
  } from "$lib/ipc";

  interface Props {
    prompts: Prompt[];
    /** Called after anything that changes the prompt list. */
    onLibraryChanged: () => void | Promise<void>;
    onNotice: (msg: string) => void;
    onError: (msg: string) => void;
  }
  let { prompts, onLibraryChanged, onNotice, onError }: Props = $props();

  let config = $state<AppConfig | null>(null);
  let sources = $state<SourceStatus[]>([]);
  let setlist = $state<SetlistEntry[]>([]);
  let busy = $state<string | null>(null);
  // Source updates are fetched at startup but deliberately not applied — a
  // third party's edits must not appear in a live app unannounced.
  let pending = $state<PendingChange[]>([]);
  let unlistenUpdated: UnlistenFn | null = null;

  // Add-source form.
  let newRepo = $state("");
  let newRef = $state("");
  let newSubdir = $state("");

  const NEWLINE_MODES: { value: NewlineMode; label: string; hint: string }[] = [
    {
      value: "shift-enter",
      label: "Shift+Enter",
      hint: "Chat apps — Claude, ChatGPT, Slack",
    },
    {
      value: "backslash-enter",
      label: "Backslash + Enter",
      hint: "Terminal agents — Claude Code, shells",
    },
    { value: "plain", label: "Plain Enter", hint: "Editors where Enter is a line break" },
  ];

  const DISPLAY_MODES: { value: PickerDisplay; label: string; hint: string }[] = [
    {
      value: "auto",
      label: "Automatic",
      hint: "Your own screen when a second display is extended",
    },
    { value: "builtin", label: "Primary display", hint: "Always your own screen" },
    { value: "cursor", label: "Follow the cursor", hint: "Wherever the pointer is" },
  ];

  async function loadAll() {
    try {
      config = await ipc.getConfig();
      sources = await ipc.listSources();
      setlist = await ipc.getSetlist();
      pending = await ipc.sourcePendingChanges();
    } catch (e) {
      onError(`Couldn't read configuration — ${fmtErr(e)}`);
    }
  }

  onMount(async () => {
    await loadAll();
    // The backend emits this after a startup refresh finds new commits.
    unlistenUpdated = await listen("sources-updated", async () => {
      pending = await ipc.sourcePendingChanges();
    });
  });

  onDestroy(() => unlistenUpdated?.());

  async function applyUpdates() {
    busy = "apply";
    try {
      const n = await ipc.applySourceUpdates();
      pending = await ipc.sourcePendingChanges();
      await onLibraryChanged();
      onNotice(n === 1 ? "Applied 1 prompt change" : `Applied ${n} prompt changes`);
    } catch (e) {
      onError(fmtErr(e));
    } finally {
      busy = null;
    }
  }

  function changeLabel(kind: PendingChange["kind"]): string {
    if (kind === "added") return "new";
    if (kind === "removed") return "gone";
    return "edited";
  }

  async function persist(patch: Partial<AppConfig>) {
    if (!config) return;
    const next = { ...config, ...patch };
    try {
      const outcome = await ipc.saveConfig(next);
      config = next;
      if (outcome.hotkeyWarnings.length > 0) {
        // Non-fatal: the other bindings took effect.
        onError(`Saved, but ${outcome.hotkeyWarnings.join("; ")}.`);
      } else {
        onNotice(
          outcome.hotkeysRebound
            ? "Saved — hotkeys rebound, no restart needed."
            : "Saved to promptplayer.yaml",
        );
      }
    } catch (e) {
      onError(`Couldn't save configuration — ${fmtErr(e)}`);
      await loadAll();
    }
  }

  async function addSource() {
    const repo = newRepo.trim();
    if (!repo) return;
    busy = "add";
    try {
      await ipc.addSource(repo, newRef.trim() || undefined, newSubdir.trim() || undefined);
      newRepo = "";
      newRef = "";
      newSubdir = "";
      await loadAll();
      await onLibraryChanged();
      onNotice("Source added. Its prompts stay off until you enable them.");
    } catch (e) {
      onError(fmtErr(e));
    } finally {
      busy = null;
    }
  }

  async function removeSource(s: SourceStatus) {
    busy = s.id;
    try {
      await ipc.removeSource(s.id);
      await loadAll();
      await onLibraryChanged();
      onNotice(`Removed ${s.repo}`);
    } catch (e) {
      onError(fmtErr(e));
    } finally {
      busy = null;
    }
  }

  async function refreshSources() {
    busy = "refresh";
    try {
      sources = await ipc.refreshSources();
      pending = await ipc.sourcePendingChanges();
      await onLibraryChanged();
      onNotice("Sources refreshed");
    } catch (e) {
      onError(fmtErr(e));
    } finally {
      busy = null;
    }
  }

  async function importAgentDir() {
    const candidates = await ipc.agentImportCandidates();
    const picked = await openDirDialog({
      directory: true,
      multiple: false,
      title: "Choose a project to import agent prompts from",
      defaultPath: candidates[0],
    });
    if (typeof picked !== "string") return;
    busy = "import";
    try {
      const summary = await ipc.importAgentPrompts(picked);
      await onLibraryChanged();
      const parts = [`${summary.imported.length} imported`];
      if (summary.skipped > 0) parts.push(`${summary.skipped} already present`);
      if (summary.errors.length > 0) parts.push(`${summary.errors.length} skipped`);
      onNotice(parts.join(", "));
      for (const e of summary.errors) console.warn("agent import:", e);
    } catch (e) {
      onError(fmtErr(e));
    } finally {
      busy = null;
    }
  }

  async function captureLastTyped() {
    busy = "capture";
    try {
      const p = await ipc.captureLastTyped();
      await onLibraryChanged();
      onNotice(`Captured “${p.name}” — review it, then enable it.`);
    } catch (e) {
      onError(fmtErr(e));
    } finally {
      busy = null;
    }
  }

  async function addToSetlist(id: string) {
    if (!config) return;
    await persist({ setlist: [...config.setlist, id] });
    setlist = await ipc.getSetlist();
  }

  async function removeFromSetlist(index: number) {
    if (!config) return;
    const next = config.setlist.filter((_, i) => i !== index);
    await persist({ setlist: next });
    setlist = await ipc.getSetlist();
  }

  async function moveInSetlist(index: number, delta: number) {
    if (!config) return;
    const next = [...config.setlist];
    const target = index + delta;
    if (target < 0 || target >= next.length) return;
    [next[index], next[target]] = [next[target], next[index]];
    await persist({ setlist: next });
    setlist = await ipc.getSetlist();
  }

  async function resetCues() {
    await ipc.resetSetlist();
    setlist = await ipc.getSetlist();
    onNotice("Setlist rewound to the first cue");
  }

  // Prompts not already in the setlist, for the add dropdown.
  const addable = $derived(
    prompts.filter((p) => !(config?.setlist ?? []).includes(p.id)),
  );
</script>

<section class="companion">
  <header class="companion-head">
    <h3>Companion</h3>
    <p class="sub">
      These settings live in <code>promptplayer.yaml</code> — editing the file by
      hand works too.
    </p>
  </header>

  {#if !config}
    <p class="loading">Loading…</p>
  {:else}
    <!-- ── Agent prompts ────────────────────────────────────────────── -->
    <div class="block">
      <div class="block-head">
        <h4>Agent prompts</h4>
        <span class="hint">
          Import <code>.claude/commands</code>, skills, Cursor rules, and
          Continue or Copilot prompt files from a project.
        </span>
      </div>
      <div class="row">
        <button onclick={importAgentDir} disabled={busy === "import"}>
          {busy === "import" ? "Importing…" : "Import from a project…"}
        </button>
        <button onclick={captureLastTyped} disabled={busy === "capture"}>
          Save what I just typed
        </button>
      </div>
    </div>

    <!-- ── Setlist ──────────────────────────────────────────────────── -->
    <div class="block">
      <div class="block-head">
        <h4>Setlist</h4>
        <span class="hint">
          Ordered cues. One hotkey fires the next one, so you don't have to
          recall a trigger on stage.
        </span>
      </div>
      {#if setlist.length === 0}
        <p class="empty">No cues yet.</p>
      {:else}
        <ol class="setlist">
          {#each setlist as entry, i (entry.promptId + i)}
            <li class:next={entry.isNext} class:missing={entry.missing}>
              <span class="cue-name">
                {entry.name}
                {#if entry.missing}<em class="warn">(deleted)</em>{/if}
              </span>
              <span class="cue-actions">
                {#if entry.isNext}<span class="badge">next</span>{/if}
                <button title="Move up" onclick={() => moveInSetlist(i, -1)} disabled={i === 0}>↑</button>
                <button title="Move down" onclick={() => moveInSetlist(i, 1)} disabled={i === setlist.length - 1}>↓</button>
                <button title="Remove" onclick={() => removeFromSetlist(i)}>×</button>
              </span>
            </li>
          {/each}
        </ol>
        <div class="row">
          <button onclick={resetCues}>Rewind to first cue</button>
        </div>
      {/if}
      {#if addable.length > 0}
        <div class="row">
          <select
            aria-label="Add a prompt to the setlist"
            onchange={(e) => {
              const id = e.currentTarget.value;
              if (id) addToSetlist(id);
              e.currentTarget.value = "";
            }}
          >
            <option value="">Add a cue…</option>
            {#each addable as p (p.id)}
              <option value={p.id}>{p.name}</option>
            {/each}
          </select>
        </div>
      {/if}
    </div>

    <!-- ── Shared sources ───────────────────────────────────────────── -->
    <div class="block">
      <div class="block-head">
        <h4>Shared prompt sources</h4>
        <span class="hint">
          Public GitHub repositories. Prompts load read-only and stay disabled
          until you enable them, and a remote <code>hotkey:</code> is ignored.
        </span>
      </div>
      {#if pending.length > 0}
        <div class="pending" role="status">
          <div class="pending-head">
            <strong>
              {pending.length === 1
                ? "1 prompt changed upstream"
                : `${pending.length} prompts changed upstream`}
            </strong>
            <button onclick={applyUpdates} disabled={busy === "apply"}>
              {busy === "apply" ? "Applying…" : "Apply"}
            </button>
          </div>
          <ul class="pending-list">
            {#each pending.slice(0, 8) as change (change.promptId)}
              <li>
                <span class="badge">{changeLabel(change.kind)}</span>
                {change.name}
              </li>
            {/each}
            {#if pending.length > 8}
              <li class="more">and {pending.length - 8} more…</li>
            {/if}
          </ul>
          <span class="hint">
            Fetched, not applied — nothing changes in a live demo until you say so.
          </span>
        </div>
      {/if}
      {#if sources.length > 0}
        <ul class="sources">
          {#each sources as s (s.id)}
            <li>
              <span class="src-main">
                <button
                  class="link"
                  onclick={() => ipc.openExternal(s.htmlUrl)}
                  title="Open on GitHub"
                >{s.pack?.name ?? s.repo}</button>
                {#if s.gitRef}<code class="ref">@{s.gitRef}</code>{/if}
                {#if s.subdir}<code class="ref">/{s.subdir}</code>{/if}
              </span>
              <span class="src-meta">
                {#if s.manifest}
                  <span class="badge">{s.manifest.promptCount} prompts</span>
                  <code class="sha" title={`Fetched ${s.manifest.fetchedAt}`}>
                    {s.manifest.sha.slice(0, 7)}
                  </code>
                {:else}
                  <span class="badge warn">not fetched</span>
                {/if}
                <button title="Remove" onclick={() => removeSource(s)} disabled={busy === s.id}>×</button>
              </span>
            </li>
          {/each}
        </ul>
        <div class="row">
          <button onclick={refreshSources} disabled={busy === "refresh"}>
            {busy === "refresh" ? "Refreshing…" : "Refresh all"}
          </button>
        </div>
      {/if}
      <div class="row add-source">
        <input bind:value={newRepo} placeholder="owner/repo or a GitHub URL" />
        <input class="narrow" bind:value={newRef} placeholder="branch" />
        <input class="narrow" bind:value={newSubdir} placeholder="subdir" />
        <button onclick={addSource} disabled={!newRepo.trim() || busy === "add"}>
          {busy === "add" ? "Adding…" : "Add"}
        </button>
      </div>
    </div>

    <!-- ── Typing / safety behaviour ────────────────────────────────── -->
    <div class="block">
      <div class="block-head">
        <h4>Behaviour</h4>
      </div>
      <label class="field">
        <span class="field-label">Line breaks</span>
        <select
          value={config["newline-mode"]}
          onchange={(e) => persist({ "newline-mode": e.currentTarget.value as NewlineMode })}
        >
          {#each NEWLINE_MODES as m (m.value)}
            <option value={m.value}>{m.label}</option>
          {/each}
        </select>
        <span class="field-hint">
          {NEWLINE_MODES.find((m) => m.value === config?.["newline-mode"])?.hint}
        </span>
      </label>

      <label class="field">
        <span class="field-label">Palette display</span>
        <select
          value={config["picker-display"]}
          onchange={(e) => persist({ "picker-display": e.currentTarget.value as PickerDisplay })}
        >
          {#each DISPLAY_MODES as m (m.value)}
            <option value={m.value}>{m.label}</option>
          {/each}
        </select>
        <span class="field-hint">
          {DISPLAY_MODES.find((m) => m.value === config?.["picker-display"])?.hint}
        </span>
      </label>

      <label class="field checkbox">
        <input
          type="checkbox"
          checked={config["text-field-guard"]}
          onchange={(e) => persist({ "text-field-guard": e.currentTarget.checked })}
        />
        <span class="field-label">Refuse to type into password and non-text fields</span>
      </label>

      <label class="field checkbox">
        <input
          type="checkbox"
          checked={config["allow-git-expressions"]}
          onchange={(e) => persist({ "allow-git-expressions": e.currentTarget.checked })}
        />
        <span class="field-label">
          Allow <code>git()</code> in expressions
          <span class="field-hint">
            Read-only subcommands only, and never for prompts from a shared source.
          </span>
        </span>
      </label>

      <label class="field">
        <span class="field-label">Auto-disable after</span>
        <input
          class="narrow"
          type="number"
          min="0"
          max="600"
          value={config["auto-disarm-minutes"]}
          onchange={(e) => persist({ "auto-disarm-minutes": Number(e.currentTarget.value) || 0 })}
        />
        <span class="field-hint">minutes armed — 0 never disables</span>
      </label>

      <label class="field">
        <span class="field-label">Repository for <code>$GIT_BRANCH</code></span>
        <input
          value={(config["repo-hints"] ?? []).join(", ")}
          placeholder="~/src/my-project"
          onchange={(e) =>
            persist({
              "repo-hints": e.currentTarget.value
                .split(",")
                .map((s) => s.trim())
                .filter(Boolean),
            })}
        />
      </label>
    </div>
  {/if}
</section>

<style>
  .companion {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .companion-head h3 {
    margin: 0;
    font-size: 15px;
  }
  .companion-head .sub {
    margin: 2px 0 0;
    font-size: 12px;
    color: var(--text-secondary, #6b7280);
  }
  .block {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding-top: 12px;
    border-top: 1px solid var(--border, rgba(0, 0, 0, 0.08));
  }
  .block-head h4 {
    margin: 0;
    font-size: 13px;
  }
  .hint,
  .field-hint {
    font-size: 11px;
    color: var(--text-secondary, #6b7280);
  }
  .row {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    align-items: center;
  }
  .add-source input {
    flex: 1;
    min-width: 120px;
  }
  .narrow {
    max-width: 110px;
    flex: 0 0 auto;
  }
  .setlist,
  .sources {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .setlist li,
  .sources li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 4px 8px;
    border-radius: 6px;
    background: var(--bg-inset, rgba(0, 0, 0, 0.04));
    font-size: 12px;
  }
  .setlist li.next {
    outline: 1px solid var(--accent, #3b82f6);
  }
  .setlist li.missing .cue-name {
    text-decoration: line-through;
    opacity: 0.7;
  }
  .cue-actions,
  .src-meta,
  .src-main {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .badge {
    font-size: 10px;
    padding: 1px 6px;
    border-radius: 999px;
    background: var(--bg-badge, rgba(0, 0, 0, 0.08));
  }
  .badge.warn,
  .warn {
    color: #b45309;
  }
  .ref,
  .sha {
    font-size: 10px;
    opacity: 0.75;
  }
  .link {
    background: none;
    border: none;
    padding: 0;
    color: var(--accent, #3b82f6);
    cursor: pointer;
    font: inherit;
    text-decoration: underline;
  }
  .field {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    font-size: 12px;
  }
  .field-label {
    min-width: 190px;
  }
  .field.checkbox .field-label {
    min-width: 0;
  }
  .field input:not([type="checkbox"]),
  .field select {
    flex: 0 1 220px;
  }
  .empty,
  .loading {
    margin: 0;
    font-size: 12px;
    color: var(--text-secondary, #6b7280);
  }

  .pending {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 8px 10px;
    border-radius: 6px;
    background: color-mix(in srgb, var(--accent, #3b82f6) 12%, transparent);
    font-size: 12px;
  }
  .pending-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }
  .pending-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .pending-list li {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .pending-list .more {
    opacity: 0.7;
  }
</style>
