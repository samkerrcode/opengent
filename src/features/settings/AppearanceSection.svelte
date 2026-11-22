<script lang="ts">
  import { describeError } from "$lib/ipc/commands";
  import type { Theme } from "$lib/generated/bindings/Theme";
  import { settingsStore } from "./settings-store.svelte";
  import SettingsGroup from "./SettingsGroup.svelte";
  import SettingRow from "./SettingRow.svelte";

  // The theme is applied to the window by Rust once stored, so the change shows at once
  // everywhere (styles, code highlighting, server icons, title bar) without a restart.
  const OPTIONS: { value: Theme; label: string }[] = [
    { value: "system", label: "System" },
    { value: "light", label: "Light" },
    { value: "dark", label: "Dark" },
  ];

  let error = $state<string | null>(null);
  const theme = $derived(settingsStore.settings?.appearance.theme ?? "system");

  async function choose(value: Theme) {
    if (value === theme) return;
    error = null;
    try {
      await settingsStore.update({ appearance: { theme: value } });
    } catch (e) {
      error = describeError(e);
    }
  }
</script>

<SettingsGroup title="Theme">
  <SettingRow label="Color mode" description="System follows your OS light or dark setting." {error}>
    <div class="choices" role="radiogroup" aria-label="Color mode">
      {#each OPTIONS as option (option.value)}
        <label class="choice">
          <input
            type="radio"
            name="theme"
            value={option.value}
            checked={theme === option.value}
            disabled={!settingsStore.settings}
            onchange={() => choose(option.value)}
          />
          <span>{option.label}</span>
        </label>
      {/each}
    </div>
  </SettingRow>
</SettingsGroup>

<style>
  .choices {
    display: inline-flex;
    gap: 2px;
    padding: 2px;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    background: var(--bg-sunken);
  }
  .choice {
    position: relative;
    display: inline-flex;
    cursor: pointer;
  }
  .choice span {
    padding: 4px 12px;
    border-radius: var(--radius-sm);
    font-size: 13px;
    color: var(--fg-muted);
  }
  /* Sibling selectors, not :has(): the baseline is Safari 15.6 (macOS 12 WKWebView). */
  .choice input:checked + span {
    background: var(--bg-elevated);
    color: var(--fg);
    box-shadow: var(--shadow-1);
  }
  .choice input:focus-visible + span {
    box-shadow: var(--focus-ring);
  }
  .choice input {
    position: absolute;
    opacity: 0;
    pointer-events: none;
  }
</style>
