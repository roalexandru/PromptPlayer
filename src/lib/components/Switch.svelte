<!--
  Apple-style green pill toggle. Used in tray-popup, library editor, and
  per-prompt rows. Replaces three near-identical inline implementations.
-->
<script lang="ts">
  interface Props {
    checked: boolean;
    onChange: (next: boolean) => void;
    title?: string;
    label?: string;
    size?: "sm" | "md";
    disabled?: boolean;
  }

  let {
    checked,
    onChange,
    title = "",
    label = "",
    size = "md",
    disabled = false,
  }: Props = $props();

  function handle(e: MouseEvent) {
    e.stopPropagation();
    e.preventDefault();
    if (disabled) return;
    onChange(!checked);
  }
</script>

<button
  type="button"
  class="pp-switch"
  class:on={checked}
  class:sm={size === "sm"}
  class:disabled
  role="switch"
  aria-checked={checked}
  aria-label={label || title}
  {title}
  onclick={handle}
>
  <span class="knob"></span>
  {#if label}
    <span class="lbl">{label}</span>
  {/if}
</button>

<style>
  .pp-switch {
    --w: 36px;
    --h: 22px;
    --kw: 18px;
    appearance: none;
    border: none;
    padding: 0;
    margin: 0;
    background: rgba(120, 120, 128, 0.28);
    position: relative;
    width: var(--w);
    height: var(--h);
    border-radius: 999px;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    transition: background var(--pp-transition-fast);
    flex-shrink: 0;
  }
  .pp-switch.sm {
    --w: 28px;
    --h: 16px;
    --kw: 12px;
  }
  .pp-switch:has(.lbl) {
    width: auto;
    padding: 0 calc(var(--w) + 6px) 0 6px;
  }
  .pp-switch.on {
    background: var(--pp-success);
  }
  .pp-switch.disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .knob {
    position: absolute;
    left: 2px;
    top: 50%;
    transform: translateY(-50%);
    width: var(--kw);
    height: var(--kw);
    background: white;
    border-radius: 50%;
    transition: left var(--pp-transition-fast);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.2);
  }
  .pp-switch.on .knob {
    left: calc(var(--w) - var(--kw) - 2px);
  }
  .lbl {
    color: var(--pp-fg);
    font-size: var(--pp-text-sm);
    margin-right: 4px;
  }
</style>
