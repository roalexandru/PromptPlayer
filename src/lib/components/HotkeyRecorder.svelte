<script lang="ts">
  import type { Prompt } from "$lib/ipc";
  import { IS_MAC, prettyMod as platPrettyMod } from "$lib/platform";

  let {
    value = $bindable<string | null>(),
    /** All prompts so we can detect conflicts. */
    prompts = [],
    /** ID of the prompt being edited (excluded from conflict check). */
    selfId,
  }: {
    value: string | null;
    prompts: Prompt[];
    selfId: string;
  } = $props();

  let recording = $state(false);
  let pendingMods = $state<Set<string>>(new Set());
  let pendingKey = $state<string | null>(null);
  let inputEl = $state<HTMLDivElement | null>(null);

  // Reserved Prompt Player globals + per-OS system shortcuts. Selected at
  // module-eval time from `IS_MAC` so we can do an O(1) lookup against the
  // normalized hotkey string and surface a precise "claimed by …" message.
  const RESERVED_MAC: Record<string, string> = {
    "cmd+shift+p": "Prompt Player arm/disarm",
    "alt+cmd+\\": "Prompt Player picker",
    "cmd+shift+escape": "Prompt Player kill-switch",
    "cmd+shift+r": "Prompt Player panic reset",
    "cmd+h": "macOS — Hide app",
    "cmd+alt+h": "macOS — Hide Others",
    "cmd+shift+h": "macOS — Show home folder",
    "cmd+q": "macOS — Quit",
    "cmd+w": "macOS — Close window",
    "cmd+shift+w": "macOS — Close all",
    "cmd+m": "macOS — Minimize",
    "cmd+alt+m": "macOS — Minimize all",
    "cmd+n": "macOS — New window",
    "cmd+shift+n": "macOS — New folder",
    "cmd+t": "macOS — New tab",
    "cmd+shift+t": "macOS — Reopen last",
    "cmd+,": "macOS — Preferences",
    "cmd+space": "macOS — Spotlight",
    "cmd+alt+space": "macOS — Finder search",
    "cmd+tab": "macOS — App switcher",
    "cmd+shift+tab": "macOS — App switcher reverse",
    "cmd+`": "macOS — Window switcher",
    "cmd+shift+3": "macOS — Screenshot full",
    "cmd+shift+4": "macOS — Screenshot region",
    "cmd+shift+5": "macOS — Screenshot tools",
    "cmd+shift+6": "macOS — Touch Bar screenshot",
    "cmd+ctrl+q": "macOS — Lock screen",
    "cmd+ctrl+f": "macOS — Fullscreen",
    "cmd+alt+esc": "macOS — Force Quit",
    "cmd+alt+d": "macOS — Toggle Dock",
    "f11": "macOS — Show desktop",
    "f12": "macOS — Show dashboard",
    "cmd+x": "macOS — Cut",
    "cmd+c": "macOS — Copy",
    "cmd+v": "macOS — Paste",
    "cmd+z": "macOS — Undo",
    "cmd+shift+z": "macOS — Redo",
    "cmd+a": "macOS — Select all",
    "cmd+s": "macOS — Save",
    "cmd+f": "macOS — Find",
    "cmd+p": "macOS — Print",
  };

  const RESERVED_WIN: Record<string, string> = {
    "ctrl+shift+p": "Prompt Player arm/disarm",
    "ctrl+alt+\\": "Prompt Player picker",
    "ctrl+alt+shift+k": "Prompt Player kill-switch",
    "ctrl+alt+shift+r": "Prompt Player panic reset",
    "ctrl+esc": "Windows — Start menu",
    "ctrl+shift+escape": "Windows — Task Manager",
    "alt+f4": "Windows — Close window",
    "alt+tab": "Windows — App switcher",
    "alt+shift+tab": "Windows — App switcher reverse",
    "win+l": "Windows — Lock",
    "win+d": "Windows — Show desktop",
    "win+e": "Windows — File Explorer",
    "win+r": "Windows — Run",
    "win+s": "Windows — Search",
    "win+i": "Windows — Settings",
    "win+a": "Windows — Action Center",
    "win+x": "Windows — Power user menu",
    "win+tab": "Windows — Task View",
    "win+left": "Windows — Snap left",
    "win+right": "Windows — Snap right",
    "win+up": "Windows — Snap up",
    "win+down": "Windows — Snap down",
    "win+plus": "Windows — Magnifier in",
    "win+minus": "Windows — Magnifier out",
    "win+shift+s": "Windows — Snip & Sketch",
    "ctrl+x": "Windows — Cut",
    "ctrl+c": "Windows — Copy",
    "ctrl+v": "Windows — Paste",
    "ctrl+z": "Windows — Undo",
    "ctrl+y": "Windows — Redo",
    "ctrl+a": "Windows — Select all",
    "ctrl+s": "Windows — Save",
    "ctrl+f": "Windows — Find",
    "ctrl+p": "Windows — Print",
    "ctrl+n": "Windows — New",
    "ctrl+t": "Windows — New tab",
    "ctrl+w": "Windows — Close tab",
    "ctrl+,": "Windows — Preferences",
  };

  const RESERVED: Record<string, string> = IS_MAC ? RESERVED_MAC : RESERVED_WIN;

  function normalize(s: string): string {
    return s
      .toLowerCase()
      .split(/[+\-\s]+/)
      .filter((p) => p.length > 0)
      .map((p) => {
        // On Mac we collapse super/win/meta to "cmd" (single primary key).
        // On Windows we keep "win" distinct from "ctrl" so reserved-list
        // collisions match correctly (Win+L vs Ctrl+L).
        if (IS_MAC) {
          if (p === "command" || p === "meta" || p === "super" || p === "win" || p === "windows") return "cmd";
        } else {
          if (p === "super" || p === "windows") return "win";
          if (p === "command" || p === "meta" || p === "cmd") return "ctrl";
        }
        if (p === "control") return "ctrl";
        if (p === "option" || p === "opt") return "alt";
        if (p === "return") return "enter";
        if (p === "esc") return "escape";
        return p;
      })
      .join("+");
  }

  let conflict = $derived.by(() => {
    if (!value) return null;
    const norm = normalize(value);
    if (RESERVED[norm]) return `Reserved for ${RESERVED[norm]}`;
    const owner = prompts.find(
      (p) => p.id !== selfId && p.hotkey && normalize(p.hotkey) === norm,
    );
    return owner ? `Already bound to "${owner.name}"` : null;
  });

  const MOD_TOKENS = IS_MAC
    ? ["cmd", "ctrl", "alt", "shift"]
    : ["ctrl", "alt", "shift", "win"];

  let invalid = $derived.by(() => {
    if (!value) return null;
    const parts = normalize(value).split("+");
    const hasMod = parts.some((p) => MOD_TOKENS.includes(p));
    const keys = parts.filter((p) => !MOD_TOKENS.includes(p));
    if (!hasMod) {
      return IS_MAC
        ? "Needs at least one modifier (⌘, ⌃, ⌥, ⇧)"
        : "Needs at least one modifier (Ctrl, Alt, Shift, Win)";
    }
    if (keys.length === 0) return "Needs a key beyond modifiers";
    // Reject non-ASCII / dead-key characters (˙, etc) that the OS hotkey
    // parser can't handle.
    const k = keys[0];
    if (k.length === 1 && /[^\x20-\x7e]/.test(k))
      return "Unsupported key — try recording again without Option held";
    return null;
  });

  function startRecording() {
    recording = true;
    pendingMods = new Set();
    pendingKey = null;
    inputEl?.focus();
  }

  function cancelRecording() {
    recording = false;
    pendingMods = new Set();
    pendingKey = null;
  }

  function commitRecording() {
    if (pendingKey && pendingMods.size > 0) {
      const parts = Array.from(pendingMods);
      parts.sort((a, b) => {
        const order = IS_MAC
          ? ["cmd", "ctrl", "alt", "shift"]
          : ["ctrl", "alt", "shift", "win"];
        return order.indexOf(a) - order.indexOf(b);
      });
      parts.push(pendingKey);
      value = parts.join("+");
    }
    recording = false;
    pendingMods = new Set();
    pendingKey = null;
  }

  function clear() {
    value = null;
    cancelRecording();
  }

  function onKeyDown(e: KeyboardEvent) {
    if (!recording) return;
    e.preventDefault();
    e.stopPropagation();

    if (e.key === "Escape") {
      cancelRecording();
      return;
    }

    const mods = new Set<string>();
    // On Mac, the Meta key is Cmd. On Windows, the Meta key is the Win key.
    if (e.metaKey) mods.add(IS_MAC ? "cmd" : "win");
    if (e.ctrlKey) mods.add("ctrl");
    if (e.altKey) mods.add("alt");
    if (e.shiftKey) mods.add("shift");
    pendingMods = mods;

    // Modifier-only keys: just track the modifier; don't commit yet.
    const key = e.key;
    const modOnly =
      key === "Meta" ||
      key === "Control" ||
      key === "Alt" ||
      key === "Shift";
    if (modOnly) {
      pendingKey = null;
      return;
    }

    // Use `event.code` for the physical key (so Option-modified keys don't
    // produce dead-key glyphs like ˙ for Opt+H). Fallback to event.key.
    let canonical: string;
    if (e.code.startsWith("Key")) {
      canonical = e.code.slice(3).toLowerCase();          // "KeyH" -> "h"
    } else if (e.code.startsWith("Digit")) {
      canonical = e.code.slice(5);                        // "Digit1" -> "1"
    } else if (e.code.startsWith("F") && /^F\d+$/.test(e.code)) {
      canonical = e.code.toLowerCase();                   // "F1" -> "f1"
    } else if (e.code === "Space") {
      canonical = "space";
    } else if (e.code === "Enter" || e.code === "Return") {
      canonical = "enter";
    } else if (e.code === "Tab") {
      canonical = "tab";
    } else if (e.code === "Escape") {
      canonical = "escape";
    } else if (e.code === "ArrowUp") {
      canonical = "up";
    } else if (e.code === "ArrowDown") {
      canonical = "down";
    } else if (e.code === "ArrowLeft") {
      canonical = "left";
    } else if (e.code === "ArrowRight") {
      canonical = "right";
    } else {
      canonical = key.toLowerCase();
    }

    pendingKey = canonical;

    // Auto-commit once we have a real key + at least one modifier.
    if (mods.size > 0) {
      // Defer slightly so the UI shows the captured state.
      setTimeout(commitRecording, 80);
    }
  }

  function prettyMod(m: string): string {
    return platPrettyMod(m);
  }
  function prettyKey(k: string): string {
    if (k === "space") return "Space";
    if (k === "enter") return "↵";
    if (k === "escape") return "Esc";
    if (k === "tab") return "⇥";
    if (k === "up") return "↑";
    if (k === "down") return "↓";
    if (k === "left") return "←";
    if (k === "right") return "→";
    return k.length === 1 ? k.toUpperCase() : k;
  }

  function prettyDisplay(): string[] {
    const order = IS_MAC
      ? ["cmd", "ctrl", "alt", "shift"]
      : ["ctrl", "alt", "shift", "win"];
    if (recording) {
      const mods = Array.from(pendingMods).sort(
        (a, b) => order.indexOf(a) - order.indexOf(b),
      );
      const out = mods.map(prettyMod);
      if (pendingKey) out.push(prettyKey(pendingKey));
      return out;
    }
    if (!value) return [];
    const parts = normalize(value).split("+");
    const mods = parts.filter((p) => MOD_TOKENS.includes(p));
    const key = parts.find((p) => !MOD_TOKENS.includes(p));
    const out = mods.map(prettyMod);
    if (key) out.push(prettyKey(key));
    return out;
  }

  let display = $derived(prettyDisplay());
</script>

<div
  class="recorder"
  bind:this={inputEl}
  role="button"
  tabindex="0"
  aria-label="Hotkey recorder"
  class:recording
  class:has-value={!!value && !recording}
  class:has-error={!!conflict || !!invalid}
  onkeydown={onKeyDown}
  onblur={() => recording && cancelRecording()}
>
  {#if recording}
    <div class="display recording-mode">
      {#if display.length === 0}
        <span class="placeholder">Press a shortcut…</span>
      {:else}
        {#each display as part}
          <kbd>{part}</kbd>
        {/each}
      {/if}
    </div>
    <button class="cancel" onclick={cancelRecording} type="button">
      Esc
    </button>
  {:else if value}
    <div class="display">
      {#each display as part}
        <kbd>{part}</kbd>
      {/each}
    </div>
    <div class="actions">
      <button class="rebind" onclick={startRecording} type="button">Rebind</button>
      <button class="clear" onclick={clear} type="button" aria-label="Clear shortcut">×</button>
    </div>
  {:else}
    <button class="set" onclick={startRecording} type="button">
      <span class="plus">+</span> Set shortcut
    </button>
  {/if}
</div>

{#if conflict}
  <small class="error">⚠ {conflict}</small>
{:else if invalid}
  <small class="error">⚠ {invalid}</small>
{:else if value}
  <small class="hint">Press Rebind to change. Esc to cancel during recording.</small>
{:else if recording}
  {#if IS_MAC}
    <small class="hint">
      Press a combo. If nothing captures, macOS is intercepting it — try a
      different key (avoid ⌘H, ⌘Q, ⌘W, ⌘⇧H, ⌘Space).
    </small>
  {:else}
    <small class="hint">
      Press a combo. If nothing captures, Windows is intercepting it — try a
      different key (avoid Ctrl+L, Alt+F4, Win+anything).
    </small>
  {/if}
{:else}
  {#if IS_MAC}
    <small class="hint">
      Click <strong>Set shortcut</strong> and press your combo. Try things like
      <kbd>⌘⇧1</kbd>, <kbd>⌃⌥1</kbd>, or <kbd>⌥F1</kbd>.
    </small>
  {:else}
    <small class="hint">
      Click <strong>Set shortcut</strong> and press your combo. Try things like
      <kbd>Ctrl+Shift+1</kbd>, <kbd>Ctrl+Alt+1</kbd>, or <kbd>Alt+F1</kbd>.
    </small>
  {/if}
{/if}

<style>
  .recorder {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    border: 1px solid rgba(0, 0, 0, 0.08);
    border-radius: 8px;
    background: rgba(255, 255, 255, 0.5);
    min-height: 36px;
    cursor: pointer;
    outline: none;
    transition: all 0.12s;
  }
  .recorder:focus,
  .recorder.recording {
    border-color: rgba(91, 141, 239, 0.5);
    background: rgba(91, 141, 239, 0.08);
    outline: 2px solid rgba(91, 141, 239, 0.25);
    outline-offset: -1px;
  }
  .recorder.has-error {
    border-color: rgba(220, 53, 69, 0.5);
  }
  .recorder.recording { cursor: text; }
  @media (prefers-color-scheme: dark) {
    .recorder {
      background: rgba(0, 0, 0, 0.25);
      border-color: rgba(255, 255, 255, 0.1);
    }
  }

  .display {
    display: flex;
    align-items: center;
    gap: 4px;
    flex: 1;
    min-height: 22px;
  }
  .recording-mode {
    color: #4a6cd4;
  }
  @media (prefers-color-scheme: dark) {
    .recording-mode { color: #88aef6; }
  }
  .placeholder {
    color: rgba(0, 0, 0, 0.4);
    font-style: italic;
    font-size: 12px;
  }
  @media (prefers-color-scheme: dark) {
    .placeholder { color: rgba(255, 255, 255, 0.45); }
  }

  kbd {
    display: inline-block;
    min-width: 22px;
    text-align: center;
    padding: 2px 6px;
    border-radius: 4px;
    background: rgba(0, 0, 0, 0.06);
    border: 1px solid rgba(0, 0, 0, 0.08);
    font-family: -apple-system, BlinkMacSystemFont, sans-serif;
    font-size: 12px;
    font-weight: 600;
    color: rgba(0, 0, 0, 0.85);
  }
  @media (prefers-color-scheme: dark) {
    kbd {
      background: rgba(255, 255, 255, 0.1);
      border-color: rgba(255, 255, 255, 0.12);
      color: rgba(255, 255, 255, 0.9);
    }
  }

  button.set {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    background: transparent;
    border: 1px dashed rgba(0, 0, 0, 0.2);
    border-radius: 6px;
    padding: 4px 12px;
    font-size: 12px;
    color: rgba(0, 0, 0, 0.55);
    font-weight: 500;
    cursor: pointer;
    transition: all 0.12s;
    margin: -2px 0;
  }
  button.set:hover {
    border-color: rgba(91, 141, 239, 0.6);
    color: #4a6cd4;
    background: rgba(91, 141, 239, 0.06);
  }
  button.set .plus {
    font-size: 14px;
    line-height: 1;
    margin-top: -1px;
  }
  @media (prefers-color-scheme: dark) {
    button.set { color: rgba(255, 255, 255, 0.55); border-color: rgba(255, 255, 255, 0.18); }
    button.set:hover { color: #88aef6; background: rgba(91, 141, 239, 0.12); }
  }

  .actions { display: flex; gap: 4px; align-items: center; }
  button.rebind, button.clear, button.cancel {
    background: rgba(0, 0, 0, 0.04);
    border: 1px solid rgba(0, 0, 0, 0.06);
    border-radius: 5px;
    padding: 3px 9px;
    font-size: 11px;
    color: rgba(0, 0, 0, 0.6);
    cursor: pointer;
    font-weight: 500;
  }
  button.rebind:hover, button.cancel:hover { background: rgba(0, 0, 0, 0.08); }
  button.clear {
    padding: 3px 7px;
    font-size: 14px;
    line-height: 1;
    color: rgba(0, 0, 0, 0.4);
  }
  button.clear:hover {
    background: rgba(220, 53, 69, 0.12);
    color: #c53030;
    border-color: rgba(220, 53, 69, 0.25);
  }
  @media (prefers-color-scheme: dark) {
    button.rebind, button.clear, button.cancel {
      background: rgba(255, 255, 255, 0.06);
      border-color: rgba(255, 255, 255, 0.1);
      color: rgba(255, 255, 255, 0.65);
    }
    button.rebind:hover, button.cancel:hover { background: rgba(255, 255, 255, 0.12); }
    button.clear:hover {
      background: rgba(220, 53, 69, 0.2);
      color: #ff8a85;
    }
  }

  small.error {
    display: block;
    color: #c53030;
    font-size: 11px;
    margin-top: 4px;
    line-height: 1.4;
  }
  small.hint {
    display: block;
    color: rgba(0, 0, 0, 0.5);
    font-size: 11px;
    margin-top: 4px;
    line-height: 1.4;
  }
  @media (prefers-color-scheme: dark) {
    small.error { color: #ff8a85; }
    small.hint { color: rgba(255, 255, 255, 0.5); }
  }
</style>
