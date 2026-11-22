// Which top-level view fills the canvas. Chats remain the default; the Agents area has a
// list/intro page and an editor (agentId null = creating a new agent).
export type View = { kind: "chat" } | { kind: "agents" } | { kind: "agent"; agentId: string | null };

export type Modal = "settings" | "providers" | "connections";

/** Sections shown inside the Settings dialog (see `features/settings/sections.ts`). */
export type SettingsPaneId = "general";
/** Everything the Settings rail lists, including the entries that open their own dialogs. */
export type SettingsSectionId = SettingsPaneId | "providers" | "connections";

class Nav {
  view = $state<View>({ kind: "chat" });
  /** A modal dialog open over the canvas; file drops are ignored while one is up. */
  modal = $state<Modal | null>(null);
  /** The Settings section shown when the Settings dialog is open. */
  settingsSection = $state<SettingsPaneId>("general");

  showChat() {
    this.view = { kind: "chat" };
  }

  showAgents() {
    this.view = { kind: "agents" };
  }

  editAgent(agentId: string | null) {
    this.view = { kind: "agent", agentId };
  }

  /** Opens Settings at a section; the Providers and Connections entries open their own dialogs. */
  openSettings(section: SettingsSectionId = this.settingsSection) {
    if (section === "providers" || section === "connections") {
      this.modal = section;
      return;
    }
    this.settingsSection = section;
    this.modal = "settings";
  }

  toggleSettings() {
    if (this.modal === "settings") this.modal = null;
    else this.openSettings();
  }

  closeModal() {
    this.modal = null;
  }
}

export const nav = new Nav();
