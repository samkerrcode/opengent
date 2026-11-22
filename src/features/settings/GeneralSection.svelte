<script lang="ts">
  import { onMount } from "svelte";
  import { describeError, type AppInfo } from "$lib/ipc/commands";
  import { loadAppInfo } from "$lib/app-info";
  import { copyText } from "$lib/clipboard";
  import SettingsGroup from "./SettingsGroup.svelte";
  import SettingRow from "./SettingRow.svelte";

  let info = $state<AppInfo | null>(null);
  let error = $state<string | null>(null);
  let copied = $state(false);
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;

  onMount(() => {
    loadAppInfo().then(
      (i) => (info = i),
      (e) => (error = describeError(e)),
    );
    return () => clearTimeout(copiedTimer);
  });

  async function copyPath() {
    if (!info || !(await copyText(info.data_root))) return;
    copied = true;
    clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => (copied = false), 1500);
  }
</script>

<SettingsGroup title="About">
  {#if info}
    <SettingRow label="Version">
      <span class="value">{info.product.version}{info.debug_build ? " (debug build)" : ""}</span>
    </SettingRow>
    <SettingRow label="Data folder" description="Chats, attachments and settings are stored here." stacked>
      <span class="value path selectable">{info.data_root}</span>
      <button class="btn-outline btn-sm" onclick={copyPath}>{copied ? "Copied" : "Copy"}</button>
    </SettingRow>
    <SettingRow
      label="Keychain"
      description={info.secret_backend === "memory"
        ? "The OS keychain is unavailable: API keys and tokens are kept in memory and lost when the app quits."
        : "Where API keys and tokens are stored."}
    >
      <span class="value" class:warn={info.secret_backend === "memory"}>{info.secret_backend}</span>
    </SettingRow>
  {:else if error}
    <SettingRow label="Unavailable" {error}><span></span></SettingRow>
  {/if}
</SettingsGroup>

<style>
  .value {
    color: var(--fg);
  }
  .path {
    flex: 1;
    min-width: 0;
    font-family: var(--font-mono);
    font-size: 12px;
    overflow-wrap: anywhere;
  }
  .warn {
    color: var(--danger);
  }
</style>
