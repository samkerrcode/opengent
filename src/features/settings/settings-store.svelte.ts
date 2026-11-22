// App-wide preferences (`settings.json`, owned by Rust). Controls apply immediately: the
// change shows at once and is replaced by what Rust stored, or rolled back if it failed.
import { commands, type Settings, type SettingsPatch } from "$lib/ipc/commands";

export function applyPatch(settings: Settings, patch: SettingsPatch): Settings {
  const next: Record<string, unknown> = { ...settings };
  for (const [section, fields] of Object.entries(patch)) {
    next[section] = { ...(settings as Record<string, object>)[section], ...fields };
  }
  return next as Settings;
}

class SettingsStore {
  settings = $state<Settings | null>(null);

  async load() {
    this.settings = await commands.getSettings();
  }

  /** Rejects with the error to show next to the control, after reverting to the stored value. */
  async update(patch: SettingsPatch): Promise<void> {
    const before = this.settings;
    if (before) this.settings = applyPatch(before, patch);
    try {
      this.settings = await commands.updateSettings(patch);
    } catch (e) {
      // Re-read rather than restore `before`: another update may have landed meanwhile.
      this.settings = await commands.getSettings().catch(() => before);
      throw e;
    }
  }
}

export const settingsStore = new SettingsStore();
