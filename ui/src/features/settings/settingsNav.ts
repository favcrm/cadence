import type { SettingsSection } from "../../lib/router";

export type SettingsIcon =
  | "preferences" | "security" | "connections" | "email" | "templates" | "general"
  | "status" | "agents" | "memory" | "permissions" | "custody" | "account" | "members" | "documents";

export type SettingsNavEntry = { section: SettingsSection; label: string; icon: SettingsIcon };
export type SettingsNavGroupSpec = { label: string; items: SettingsNavEntry[] };

const ENTRIES = {
  preferences: { section: "preferences", label: "Preferences", icon: "preferences" },
  /** CAD-2578: hosted only — offered when the board names `security` in its sections. */
  security: { section: "security", label: "Security", icon: "security" },
  connections: { section: "connections", label: "Connections", icon: "connections" },
  email: { section: "email", label: "Email sending", icon: "email" },
  templates: { section: "templates", label: "Templates", icon: "templates" },
  general: { section: "general", label: "General", icon: "general" },
  status: { section: "update", label: "Status", icon: "status" },
  agents: { section: "models", label: "Agents", icon: "agents" },
  memory: { section: "memory", label: "Memory", icon: "memory" },
  permissions: { section: "permissions", label: "Master permissions", icon: "permissions" },
  custody: { section: "custody", label: "Advanced custody", icon: "custody" },
  account: { section: "account", label: "Billing", icon: "account" },
  /** CAD-2115: hosted only — a native board has no tenant member list, so this
   *  entry is offered only by a board that names `members` in its sections. */
  members: { section: "members", label: "Members", icon: "members" },
  /** CAD-2286: hosted only — offered by a board that names `documents`, which
   *  the hosted board does while the document lifecycle is on. */
  documents: { section: "documents", label: "Documents", icon: "documents" },
} satisfies Record<string, SettingsNavEntry>;

/** The fields of `/api/meta` that decide what Settings offers. */
export type SettingsMeta = {
  settings_sections?: readonly string[];
  hosted?: boolean;
  platform_account_configured?: boolean;
  wiki_managed_files?: boolean;
} | null | undefined;

/**
 * Which sections to offer. A board that sends `settings_sections` has said what
 * it serves, so only those appear. One that doesn't (a native board, or a hosted
 * board that predates the field) keeps the full list — which has no `members`:
 * a native board has no tenant member list to show. CAD-2397: it does have
 * General, which holds the company time zone.
 */
export function settingsNavGroups(meta: SettingsMeta): SettingsNavGroupSpec[] {
  const account = meta?.platform_account_configured ? [ENTRIES.account] : [];
  const listed = meta?.settings_sections;
  if (!listed) {
    return [
      { label: "You", items: [ENTRIES.preferences] },
      { label: "Workspace", items: [ENTRIES.agents, ENTRIES.connections, ENTRIES.email, ...(!meta?.hosted ? [ENTRIES.templates] : []), ENTRIES.general, ENTRIES.memory] },
      { label: "System", items: [ENTRIES.status, ...account, ENTRIES.permissions, ENTRIES.custody] },
    ];
  }
  const pick = (...names: (keyof typeof ENTRIES)[]) =>
    names.filter((name) => listed?.includes(name)).map((name) => ENTRIES[name]);
  return [
    { label: "You", items: pick("preferences", "security") },
    // CAD-2115: Members leads the workspace pages, as the approved mockup shows.
    // CAD-2286: Documents follows Members; Members stays first, where
    // `settingsRedirect` lands a request for a page this board does not serve.
    { label: "Workspace", items: pick("members", ...(meta?.wiki_managed_files ? [] : ["documents" as const]), "connections", "email", ...(!meta?.hosted ? ["templates" as const] : []), "general") },
    { label: "System", items: [...pick("status"), ...account] },
  ].filter((group) => group.items.length > 0);
}

/**
 * Sections only a hosted board can serve. A native board has no tenant member
 * list and no document lifecycle, so it neither offers nor mounts either.
 */
const HOSTED_ONLY: readonly SettingsSection[] = ["members", "documents", "security"];

/**
 * Where a request for `section` should land instead, or null when it is fine.
 * Nothing moves until the board has said what it serves, and an unknown list
 * refuses nothing, so a slow or old board can't lose a working page. The one
 * exception is a hosted-only section: a board that answered without it is
 * native, where that page does not exist — a direct link moves instead of
 * mounting a page that has nothing to read.
 */
export function settingsRedirect(section: SettingsSection, meta: SettingsMeta): SettingsSection | null {
  if (!meta || section === "templates") return null;
  if (section === "documents" && meta.wiki_managed_files && meta.settings_sections?.includes("general")) return "general";
  const groups = settingsNavGroups(meta);
  if (groups.some((group) => group.items.some((item) => item.section === section))) return null;
  // No list at all: a native board. Only a hosted-only page is refused there;
  // every other section is one this board still has.
  if (!meta.settings_sections && !HOSTED_ONLY.includes(section)) return null;
  // Workspace leads; a member whose board serves none of it keeps the first group.
  const workspace = groups.find((group) => group.label === "Workspace")?.items[0];
  return workspace?.section ?? groups[0]?.items[0]?.section ?? "preferences";
}

/** The current section's name for the breadcrumb. */
export function settingsLabel(section: SettingsSection): string | null {
  const entry = Object.values(ENTRIES).find((item) => item.section === section);
  return entry?.label ?? null;
}
