import type { SettingsSection } from "../../lib/router";

export type SettingsIcon =
  | "preferences" | "connections" | "email" | "general"
  | "status" | "agents" | "memory" | "permissions" | "custody" | "account";

export type SettingsNavEntry = { section: SettingsSection; label: string; icon: SettingsIcon };
export type SettingsNavGroupSpec = { label: string; items: SettingsNavEntry[] };

const ENTRIES = {
  preferences: { section: "preferences", label: "Preferences", icon: "preferences" },
  connections: { section: "connections", label: "Connections", icon: "connections" },
  email: { section: "email", label: "Email sending", icon: "email" },
  general: { section: "general", label: "General", icon: "general" },
  status: { section: "update", label: "Status", icon: "status" },
  agents: { section: "models", label: "Agents", icon: "agents" },
  memory: { section: "memory", label: "Memory", icon: "memory" },
  permissions: { section: "permissions", label: "Master permissions", icon: "permissions" },
  custody: { section: "custody", label: "Advanced custody", icon: "custody" },
  account: { section: "account", label: "Account", icon: "account" },
} satisfies Record<string, SettingsNavEntry>;

/** The fields of `/api/meta` that decide what Settings offers. */
export type SettingsMeta = {
  settings_sections?: readonly string[];
  platform_account_configured?: boolean;
} | null | undefined;

/**
 * Which sections to offer. A board that sends `settings_sections` has said what
 * it serves, so only those appear. One that doesn't (a native board, or a hosted
 * board that predates the field) keeps the full list.
 */
export function settingsNavGroups(meta: SettingsMeta): SettingsNavGroupSpec[] {
  const account = meta?.platform_account_configured ? [ENTRIES.account] : [];
  const listed = meta?.settings_sections;
  if (!listed) {
    return [
      { label: "You", items: [ENTRIES.preferences] },
      { label: "Workspace", items: [ENTRIES.agents, ENTRIES.connections, ENTRIES.email, ENTRIES.memory] },
      { label: "System", items: [ENTRIES.status, ...account, ENTRIES.permissions, ENTRIES.custody] },
    ];
  }
  const pick = (...names: (keyof typeof ENTRIES)[]) =>
    names.filter((name) => listed.includes(name)).map((name) => ENTRIES[name]);
  return [
    { label: "You", items: pick("preferences") },
    { label: "Workspace", items: pick("connections", "email", "general") },
    { label: "System", items: [...pick("status"), ...account] },
  ].filter((group) => group.items.length > 0);
}

/**
 * Where a request for `section` should land instead, or null when it is fine.
 * Nothing moves until the board has said what it serves, and an unknown list
 * refuses nothing, so a slow or old board can't lose a working page.
 */
export function settingsRedirect(section: SettingsSection, meta: SettingsMeta): SettingsSection | null {
  if (!meta?.settings_sections) return null;
  const groups = settingsNavGroups(meta);
  if (groups.some((group) => group.items.some((item) => item.section === section))) return null;
  // Agents is the bare /settings route; a board without it opens on its first working page.
  const workspace = groups.find((group) => group.label === "Workspace")?.items[0];
  return workspace?.section ?? groups[0]?.items[0]?.section ?? null;
}

/** The current section's name for the breadcrumb. */
export function settingsLabel(section: SettingsSection): string | null {
  const entry = Object.values(ENTRIES).find((item) => item.section === section);
  return entry?.label ?? null;
}
