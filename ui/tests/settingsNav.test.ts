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
  ["Preferences", "Connections", "Email sending", "General", "Status"],
  "a hosted owner sees only what the board serves",
);
equal(flat(MEMBER), ["Preferences"], "a member sees less, and nothing this build has no page for");
for (const absent of ["Agents", "Memory", "Master permissions", "Advanced custody"])
  if (flat(OWNER).includes(absent)) throw new Error(`${absent} must be hidden on a hosted board`);
equal(
  flat(null),
  ["Preferences", "Agents", "Connections", "Email sending", "Memory", "Status", "Master permissions", "Advanced custody"],
  "no list keeps every section a native board has",
);
equal(
  flat({ ...OWNER, platform_account_configured: true }).includes("Account"),
  true,
  "Account keeps its own flag",
);
equal(settingsRedirect("models", OWNER), "connections", "bare /settings opens on a working page");
equal(settingsRedirect("permissions", OWNER), "connections", "a direct link to a hidden section moves");
equal(settingsRedirect("connections", OWNER), null, "a served section stays");
equal(settingsRedirect("models", MEMBER), "preferences", "a member lands on a page they have");
equal(settingsRedirect("models", null), null, "nothing moves before the board has answered");
equal(settingsRedirect("permissions", {}), null, "a board that states nothing keeps every route");
equal(settingsLabel("update"), "Status", "the crumb names the section");
