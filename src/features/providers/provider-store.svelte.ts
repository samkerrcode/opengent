// Provider connections and the active provider/model selection. Mirrors Rust state; Rust is
// authoritative and this store re-fetches after every mutation.
import { commands, describeError, type ActiveSelection, type ProviderConfig, type ProviderInput } from "$lib/ipc/commands";

class ProviderStore {
  providers = $state<ProviderConfig[]>([]);
  active = $state<ActiveSelection | null>(null);
  loaded = $state(false);
  error = $state<string | null>(null);

  get activeProvider(): ProviderConfig | null {
    return this.providers.find((p) => p.id === this.active?.provider_id) ?? null;
  }

  async load() {
    try {
      [this.providers, this.active] = await Promise.all([commands.listProviders(), commands.getActiveSelection()]);
      if (!this.active) {
        const first = this.providers[0];
        if (first?.default_model) await this.select(first.id, first.default_model);
      }
      this.error = null;
    } catch (e) {
      this.error = describeError(e);
    } finally {
      this.loaded = true;
    }
  }

  async select(providerId: string, model: string) {
    const selection = { provider_id: providerId, model };
    await commands.setActiveSelection(selection);
    this.active = selection;
  }

  async save(input: ProviderInput): Promise<ProviderConfig> {
    const saved = await commands.saveProvider(input);
    await this.load();
    return saved;
  }

  async remove(id: string) {
    await commands.deleteProvider(id);
    await this.load();
  }

  async test(id: string) {
    const result = await commands.testProvider(id);
    await this.load();
    return result;
  }
}

export const providerStore = new ProviderStore();
