<script lang="ts">
  import { onMount } from "svelte";
  import { describeError, type AppInfo } from "$lib/ipc/commands";
  import { loadAppInfo } from "$lib/app-info";
  import { copyText } from "$lib/clipboard";
  import Switch from "$lib/components/Switch.svelte";
  import { settingsStore } from "./settings-store.svelte";
  import SettingsGroup from "./SettingsGroup.svelte";
  import SettingRow from "./SettingRow.svelte";

  let info = $state<AppInfo | null>(null);
  let error = $state<string | null>(null);
  let shareError = $state<string | null>(null);

  async function setShareEnvironment(on: boolean) {
    shareError = null;
    try {
      await settingsStore.update({ general: { share_environment: on } });
    } catch (e) {
      shareError = describeError(e);
    }
  }
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

<SettingsGroup title="Models">
  <SettingRow
    label="Share environment with models"
    description="Tells models your OS version, time zone and the local time each of your messages was sent, so they can answer questions about dates and give advice for your system. Applies to replies started after a change; turning it on also sends the send times of a chat's earlier messages."
    error={shareError}
  >
    {#if settingsStore.settings}
      <Switch
        checked={settingsStore.settings.general.share_environment}
        onchange={setShareEnvironment}
        label="Share environment with models"
      />
    {/if}
  </SettingRow>
</SettingsGroup>

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
