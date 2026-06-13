<script lang="ts">
  // §10.2 — live cadence preview using the same statistical model as the
  // backend typer (log-normal mixture + hierarchical pauses).

  import { onDestroy } from "svelte";
  import type { TypingOverrides } from "$lib/ipc";

  let {
    body,
    profile = "sales-engineer",
    overrides = null,
  }: {
    body: string;
    profile?: string;
    /** Per-prompt typing overrides — folded into `custom` profile params. */
    overrides?: Partial<TypingOverrides> | null;
  } = $props();

  let preview = $state("");
  let playing = $state(false);
  let phase = $state<"idle" | "pre-typing" | "typing" | "done">("idle");
  let cancelToken = $state(0);

  // Stop the playback loop when the component unmounts — otherwise the
  // async for-loop keeps mutating state on a dead component.
  onDestroy(() => {
    cancelToken++;
  });

  function lognormal(mu: number, sigma: number): number {
    const u1 = Math.random() || 1e-9;
    const u2 = Math.random();
    const z = Math.sqrt(-2 * Math.log(u1)) * Math.cos(2 * Math.PI * u2);
    return Math.exp(mu + sigma * z);
  }
  // Mirror profiles.rs so the JS preview matches the Rust engine.
  function profileParams(p: string): { ikiScale: number; pauseScale: number; varianceScale: number } {
    if (p === "fast-presenter") return { ikiScale: 0.22, pauseScale: 0.4, varianceScale: 0.4 };
    if (p === "thoughtful-ceo") return { ikiScale: 0.85, pauseScale: 1.3, varianceScale: 1.5 };
    const base = { ikiScale: 0.5, pauseScale: 0.9, varianceScale: 1.0 };
    if (p === "custom" && overrides) {
      // sampleIki()'s main mode has median ≈ e^4.95 ≈ 140 ms, so scaling by
      // (requested median / 140) makes the preview's median IKI track the
      // override the way the Rust engine does.
      const iki = overrides["iki-median-ms"];
      if (iki != null && iki > 0) base.ikiScale = iki / 140;
      const pv = overrides["pause-variance-scale"];
      if (pv != null && pv > 0) base.varianceScale = pv;
    }
    return base;
  }
  function sampleIki(): number {
    const m =
      Math.random() < 0.15 ? lognormal(6.2, 0.5) : lognormal(4.95, 0.35);
    return Math.min(3000, Math.max(60, m));
  }
  function sleep(ms: number) {
    return new Promise<void>((r) => setTimeout(r, ms));
  }

  async function start() {
    if (playing) {
      cancelToken++;
      playing = false;
      phase = "idle";
      return;
    }
    const myToken = ++cancelToken;
    playing = true;
    phase = "pre-typing";
    preview = "";
    const params = profileParams(profile);

    // Snappier pre-typing for the preview vs the real engine's 1.5s.
    await sleep(400 + Math.random() * 200);
    if (myToken !== cancelToken) return;
    phase = "typing";

    let prev = "";
    for (const c of body) {
      if (myToken !== cancelToken) return;
      let iki = sampleIki() * params.ikiScale;
      // Match distributions.rs: word μ=5.2, sentence μ=6.4, paragraph mean 1500.
      if (prev === " " && c !== " ") iki += params.pauseScale * lognormal(5.2, 0.4 * params.varianceScale);
      if (prev.match(/[.!?]/) && c === " ") iki += params.pauseScale * lognormal(6.4, 0.5 * params.varianceScale);
      if (prev === "\n" && c === "\n") iki += params.pauseScale * 1500;
      await sleep(iki);
      if (myToken !== cancelToken) return;
      preview += c;
      prev = c;
    }
    phase = "done";
    playing = false;
  }

  function reset() {
    cancelToken++;
    playing = false;
    phase = "idle";
    preview = "";
  }
</script>

<div class="cad">
  <div class="controls">
    <button class="play" onclick={start}>
      {#if playing}
        <span class="ico">■</span> Stop
      {:else}
        <span class="ico">▶</span> Play preview
      {/if}
    </button>
    <button class="reset" onclick={reset} disabled={!preview && !playing}>
      Reset
    </button>
    <span class="status">
      <span class="prof">{profile}</span>
      {#if phase === "pre-typing"}
        <span class="phase">thinking…</span>
      {:else if phase === "typing"}
        <span class="phase">typing</span>
      {:else if phase === "done"}
        <span class="phase">done</span>
      {/if}
    </span>
  </div>

  <div class="screen-wrap">
    <pre class="screen" class:empty={!preview && !playing}>{preview ||
        (playing
          ? ""
          : "Click Play preview to see this prompt type out at human cadence.")}{#if playing}<span class="cursor">▍</span>{/if}</pre>
  </div>
</div>

<style>
  .cad {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .controls {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  button {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px 12px;
    border-radius: 6px;
    border: 1px solid rgba(0, 0, 0, 0.1);
    background: rgba(255, 255, 255, 0.6);
    backdrop-filter: blur(20px);
    -webkit-backdrop-filter: blur(20px);
    font-size: 12px;
    font-weight: 500;
    cursor: pointer;
    color: inherit;
    transition: all 0.12s;
  }
  button:hover {
    background: rgba(255, 255, 255, 0.8);
    border-color: rgba(0, 0, 0, 0.18);
  }
  button:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  button.play {
    background: rgba(91, 141, 239, 0.18);
    border-color: rgba(91, 141, 239, 0.35);
    color: #4a6cd4;
  }
  button.play:hover { background: rgba(91, 141, 239, 0.28); }
  .ico { font-size: 9px; }
  .status {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-left: auto;
    font-size: 11px;
    color: rgba(0, 0, 0, 0.5);
  }
  .prof {
    font-family: ui-monospace, monospace;
    font-size: 10px;
    color: rgba(0, 0, 0, 0.55);
    background: rgba(0, 0, 0, 0.05);
    padding: 1px 6px;
    border-radius: 3px;
  }
  .phase {
    font-style: italic;
    color: #4a6cd4;
  }
  .screen-wrap {
    border-radius: 8px;
    overflow: hidden;
  }
  .screen {
    margin: 0;
    padding: 16px 18px;
    min-height: 220px;
    max-height: 360px;
    overflow-y: auto;
    white-space: pre-wrap;
    word-wrap: break-word;
    font-family: ui-monospace, "SF Mono", Menlo, monospace;
    font-size: 13px;
    line-height: 1.6;
    background: rgba(255, 255, 255, 0.5);
    color: rgba(0, 0, 0, 0.92);
    border: 1px solid rgba(0, 0, 0, 0.08);
    backdrop-filter: blur(20px);
    -webkit-backdrop-filter: blur(20px);
  }
  .screen.empty {
    color: rgba(0, 0, 0, 0.4);
    font-style: italic;
    font-family:
      -apple-system, BlinkMacSystemFont, "SF Pro", "Helvetica Neue", sans-serif;
  }
  .cursor {
    display: inline-block;
    color: #4a6cd4;
    animation: blink 1s steps(2) infinite;
  }
  @keyframes blink {
    50% { opacity: 0; }
  }

  @media (prefers-color-scheme: dark) {
    button {
      background: rgba(255, 255, 255, 0.08);
      border-color: rgba(255, 255, 255, 0.12);
    }
    button:hover {
      background: rgba(255, 255, 255, 0.16);
      border-color: rgba(255, 255, 255, 0.2);
    }
    button.play {
      background: rgba(91, 141, 239, 0.22);
      border-color: rgba(91, 141, 239, 0.4);
      color: #88aef6;
    }
    .status, .prof { color: rgba(255, 255, 255, 0.55); }
    .prof { background: rgba(255, 255, 255, 0.08); }
    .phase { color: #88aef6; }
    .screen {
      background: rgba(0, 0, 0, 0.3);
      color: rgba(255, 255, 255, 0.95);
      border-color: rgba(255, 255, 255, 0.08);
    }
    .screen.empty { color: rgba(255, 255, 255, 0.4); }
    .cursor { color: #88aef6; }
  }
</style>
