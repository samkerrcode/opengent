<script lang="ts">
  import type { Snippet } from "svelte";

  // On/off toggle: a native checkbox with `role="switch"`, so keyboard and screen-reader
  // behaviour stay native. `children` is an optional visible label next to the track.
  // Controlled: a click only reports the new value, and the switch shows `checked`. Svelte
  // only writes `checked` when the prop changes, so a failed action would otherwise leave
  // the box flipped.
  function toggle(e: Event & { currentTarget: HTMLInputElement }) {
    const input = e.currentTarget;
    onchange(input.checked);
    input.checked = checked;
  }
  let {
    checked,
    onchange,
    label,
    title,
    disabled = false,
    small = false,
    children,
  }: {
    checked: boolean;
    onchange: (checked: boolean) => void;
    /** Accessible name when there is no visible label. */
    label?: string;
    title?: string;
    disabled?: boolean;
    small?: boolean;
    children?: Snippet;
  } = $props();
</script>

<label class="switch" class:small {title}>
  <input type="checkbox" role="switch" {checked} {disabled} aria-label={label} onchange={toggle} />
  {#if children}<span class="text">{@render children()}</span>{/if}
</label>

<style>
  .switch {
    display: inline-flex;
    flex-direction: row;
    align-items: center;
    gap: 8px;
    flex: none;
    min-width: 0;
    cursor: pointer;
    color: var(--fg);
  }
  .text {
    min-width: 0;
  }
  input {
    -webkit-appearance: none;
    appearance: none;
    margin: 0;
    width: 30px;
    height: 17px;
    border-radius: 999px;
    background: var(--border-strong);
    position: relative;
    cursor: pointer;
    transition: background var(--motion-fast) var(--ease);
    flex: none;
  }
  input::after {
    content: "";
    position: absolute;
    top: 2px;
    left: 2px;
    width: 13px;
    height: 13px;
    border-radius: 50%;
    background: #fff;
    transition: transform var(--motion-fast) var(--ease);
  }
  input:checked {
    background: var(--accent);
  }
  input:checked::after {
    transform: translateX(13px);
  }
  input:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  input:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .small input {
    width: 24px;
    height: 14px;
  }
  .small input::after {
    width: 10px;
    height: 10px;
  }
  .small input:checked::after {
    transform: translateX(10px);
  }
</style>
