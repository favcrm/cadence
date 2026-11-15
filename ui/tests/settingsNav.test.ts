export {};
/**
 * CAD-2114: Settings offers what the board says it serves. A board that says
 * nothing keeps the full list, so deploy order can't hide a working page.
 */
import { settingsLabel, settingsNavGroups, settingsRedirect } from "../src/features/settings/settingsNav";

function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual), e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}
const flat = (meta: Parameters<typeof settingsNavGroups>[0]) =>
  settingsNavGroups(meta).flatMap((group) => group.items.map((item) => item.label));

const OWNER = { settings_sections: ["preferences", "general", "members", "connections", "email", "status"] };
const MEMBER = { settings_sections: ["preferences", "members"] };

equal(
  flat(OWNER),
  ["Preferences", "Members", "Connections", "Email sending", "General", "Status"],
  "a hosted owner sees only what the board serves",
);
equal(flat(MEMBER), ["Preferences", "Members"], "a member can reach the read-only directory");
for (const absent of ["Agents", "Memory", "Master permissions", "Advanced custody"])
  if (flat(OWNER).includes(absent)) throw new Error(`${absent} must be hidden on a hosted board`);
equal(
  flat(null),
  ["Preferences", "Agents", "Connections", "Email sending", "Templates", "General", "Memory", "Status", "Master permissions", "Advanced custody"],
  "no list keeps every section a native board has, General (company time zone) included",
);
equal(
  flat({ ...OWNER, settings_sections: [...OWNER.settings_sections, "templates"] }).includes("Templates"),
  true,
  "a hosted board that lists templates shows the sidebar entry",
);
equal(
  flat({ ...OWNER, platform_account_configured: true }).includes("Account"),
  true,
  "Account keeps its own flag",
);
equal(settingsRedirect("models", OWNER), "members", "bare /settings opens on a working page");
equal(settingsRedirect("permissions", OWNER), "members", "a direct link to a hidden section moves");
equal(settingsRedirect("connections", OWNER), null, "a served section stays");
equal(settingsRedirect("models", MEMBER), "members", "a member lands on a page they have");
equal(settingsRedirect("models", null), null, "nothing moves before the board has answered");
equal(settingsRedirect("permissions", {}), null, "a board that states nothing keeps every route");
equal(settingsRedirect("members", {}), "models", "a native board does not expose hosted members");
equal(settingsRedirect("members", { settings_sections: ["preferences"] }), "preferences", "a board without the capability refuses the direct link");
equal(settingsRedirect("templates", OWNER), null, "direct templates links stay reachable");
equal(settingsRedirect("templates", null), null, "templates waits cleanly before meta loads");
equal(settingsRedirect("templates", {}), null, "templates stays open on native boards");
equal(settingsLabel("update"), "Status", "the crumb names the section");
equal(settingsLabel("templates"), "Templates", "the crumb names Templates");

// CAD-2286: Settings → Documents is hosted only, offered when the board names it.
const DOCUMENTS = { settings_sections: ["preferences", "general", "members", "documents", "connections", "email", "status"] };
equal(
  flat(DOCUMENTS),
  ["Preferences", "Members", "Documents", "Connections", "Email sending", "General", "Status"],
  "a board that serves the lifecycle offers Documents after Members",
);
equal(flat(OWNER).includes("Documents"), false, "a board that does not name documents hides it");
equal(flat(null).includes("Documents"), false, "a native board has no Documents section");
equal(settingsRedirect("documents", DOCUMENTS), null, "a listed documents section stays");
equal(settingsRedirect("documents", OWNER), "members", "an unlisted documents section moves away");
equal(settingsRedirect("documents", {}), "models", "a native board does not expose hosted documents");
equal(settingsRedirect("documents", { settings_sections: ["preferences"] }), "preferences", "a board without the capability refuses the direct link");
equal(settingsLabel("documents"), "Documents", "the crumb names the section");
