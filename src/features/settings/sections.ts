// The Settings rail. Each feature that owns settings adds one entry here (and its id to
// `SettingsPaneId` in nav), backed by a component in its own feature folder; the values
// live in the matching section of the Rust `Settings` struct. Order here is rail order.
import type { Component } from "svelte";
import type { SettingsPaneId, SettingsSectionId } from "$lib/state/nav.svelte";
import AppearanceSection from "./AppearanceSection.svelte";
import GeneralSection from "./GeneralSection.svelte";

type Entry = { label: string; icon: string; description: string };

/** A section rendered inside the Settings dialog. */
export type SettingsPane = Entry & {
  kind: "pane";
  id: SettingsPaneId;
  component: Component;
  /** Hidden while it has nothing to show (e.g. a platform-only feature). Default: shown. */
  visible?: () => boolean;
};

/** An entry that opens an existing dialog of its own. */
export type SettingsLink = Entry & { kind: "link"; id: Exclude<SettingsSectionId, SettingsPaneId> };

export type SettingsSection = SettingsPane | SettingsLink;

const SECTIONS: readonly SettingsSection[] = [
  {
    kind: "pane",
    id: "general",
    label: "General",
    icon: "⚙︎",
    description: "What models are told about your system, about this app and where it keeps your data.",
    component: GeneralSection,
  },
  {
    kind: "pane",
    id: "appearance",
    label: "Appearance",
    icon: "◐",
    description: "Light or dark theme for this app.",
    component: AppearanceSection,
  },
  {
    kind: "link",
    id: "providers",
    label: "Providers",
    icon: "◎",
    description: "Model providers, API keys and models.",
  },
  {
    kind: "link",
    id: "connections",
    label: "Connections",
    icon: "⚒",
    description: "MCP tool servers and tool permissions.",
  },
];

export function settingsPanes(): SettingsPane[] {
  return SECTIONS.filter((s): s is SettingsPane => s.kind === "pane" && (s.visible?.() ?? true));
}

export function settingsLinks(): SettingsLink[] {
  return SECTIONS.filter((s): s is SettingsLink => s.kind === "link");
}
