import type { SettingsSection } from "../../lib/router";
import type { Meta } from "../../lib/types";
import { sessionAdmissible } from "../auth/gate";

export type SettingsIcon =
  | "preferences" | "security" | "connections" | "templates" | "general"
  | "status" | "daemons" | "agents" | "memory" | "permissions" | "custody" | "account" | "members" | "documents" | "support";

export type SettingsNavEntry = { section: SettingsSection; label: string; icon: SettingsIcon };
export type SettingsNavGroupSpec = { label: string; items: SettingsNavEntry[] };

const ENTRIES = {
  preferences: { section: "preferences", label: "Preferences", icon: "preferences" },
  /** CAD-2578: hosted only — offered when the board names `security` in its sections. */
  security: { section: "security", label: "Security", icon: "security" },
  support: { section: "support", label: "Technical support", icon: "support" },
  connections: { section: "connections", label: "Connections", icon: "connections" },
  templates: { section: "templates", label: "Templates", icon: "templates" },
  general: { section: "general", label: "General", icon: "general" },
  daemons: { section: "daemons", label: "Daemons", icon: "daemons" },
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
  signed_in?: Meta["signed_in"];
  session?: Meta["session"];
} | null | undefined;

function offersBilling(meta: SettingsMeta): boolean {
  if (meta?.platform_account_configured !== true) return false;
  if (meta.hosted !== true && meta.session?.origin !== "public") return true;
  // Hosted meta maps the current tenant's owner to the public session's
  // operator role. A platform operator token is not a workspace owner.
  const session = meta.session;
  return meta.signed_in === true && session?.origin === "public" && sessionAdmissible(session)
    && session.user?.role === "operator" && !session.user.sub.startsWith("operator-token:");
}

/**
 * Which sections to offer. A board that sends `settings_sections` has said what
 * it serves, so only those appear. One that doesn't (a native board, or a hosted
 * board that predates the field) keeps the full list — which has no `members`:
 * a native board has no tenant member list to show. CAD-2397: it does have
 * General, which holds the company time zone.
 */
export function settingsNavGroups(meta: SettingsMeta): SettingsNavGroupSpec[] {
  const account = offersBilling(meta) ? [ENTRIES.account] : [];
  const listed = meta?.settings_sections;
  if (!listed) {
    return [
      { label: "You", items: [ENTRIES.preferences] },
      { label: "Workspace", items: [ENTRIES.agents, ENTRIES.connections, ...(!meta?.hosted ? [ENTRIES.templates] : []), ENTRIES.general, ENTRIES.memory] },
      { label: "System", items: [ENTRIES.status, ...account, ENTRIES.permissions, ENTRIES.custody] },
    ];
  }
  const pick = (...names: (keyof typeof ENTRIES)[]) =>
    names.filter((name) => listed?.includes(name)).map((name) => ENTRIES[name]);
  return [
    { label: "You", items: [...pick("preferences", "security"), ...((meta?.hosted === true || meta?.session?.origin === "public") && meta?.signed_in === true ? pick("support") : [])] },
    // CAD-2115: Members leads the workspace pages, as the approved mockup shows.
    // CAD-2286: Documents follows Members; Members stays first, where
    // `settingsRedirect` lands a request for a page this board does not serve.
    { label: "Workspace", items: pick("members", ...(meta?.wiki_managed_files ? [] : ["documents" as const]), "connections", ...(!meta?.hosted ? ["templates" as const] : []), "general") },
    { label: "System", items: [...pick("daemons", "status"), ...account] },
  ].filter((group) => group.items.length > 0);
}

/**
 * Sections only a hosted board can serve. A native board has no tenant member
 * list, no document lifecycle, and no owner daemon directory, so it neither
 * offers nor mounts them.
 */
const HOSTED_ONLY: readonly SettingsSection[] = ["members", "documents", "security", "daemons", "support"];

/**
 * Where a request for `section` should land instead, or null when it is fine.
 * Wait for metadata before redirecting. An old board without a section
 * list keeps its routes except hosted-only pages and restricted hosted Billing.
 */
export function settingsRedirect(section: SettingsSection, meta: SettingsMeta): SettingsSection | null {
  if (!meta || section === "templates") return null;
  if (section === "documents" && meta.wiki_managed_files && meta.settings_sections?.includes("general")) return "general";
  if (section === "email" && meta.settings_sections?.includes("email")) return null;
  const groups = settingsNavGroups(meta);
  if (groups.some((group) => group.items.some((item) => item.section === section))) return null;
  if (!meta.settings_sections && !HOSTED_ONLY.includes(section)
      && !(section === "account" && (meta.hosted === true || meta.session?.origin === "public"))) return null;
  // Workspace leads; a member whose board serves none of it keeps the first group.
  const workspace = groups.find((group) => group.label === "Workspace")?.items[0];
  return workspace?.section ?? groups[0]?.items[0]?.section ?? "preferences";
}

/** The current section's name for the breadcrumb. */
export function settingsLabel(section: SettingsSection): string | null {
  const entry = Object.values(ENTRIES).find((item) => item.section === section);
  return entry?.label ?? null;
}
