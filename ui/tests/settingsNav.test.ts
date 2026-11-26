export {};
/**
 * CAD-2114: Settings offers what the board says it serves. A board that says
 * nothing keeps the full list, so deploy order can't hide a working page.
 */
import { settingsLabel, settingsNavGroups, settingsRedirect } from "../src/features/settings/settingsNav";
import type { Meta } from "../src/lib/types";

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
  ["Preferences", "Members", "Connections", "General", "Status"],
  "a hosted owner sees only what the board serves",
);
equal(flat(MEMBER), ["Preferences", "Members"], "a member can reach the read-only directory");
for (const absent of ["Agents", "Memory", "Master permissions", "Advanced custody"])
  if (flat(OWNER).includes(absent)) throw new Error(`${absent} must be hidden on a hosted board`);
equal(
  flat(null),
  ["Preferences", "Agents", "Connections", "Templates", "General", "Memory", "Status", "Master permissions", "Advanced custody"],
  "no list keeps every section a native board has, General (company time zone) included",
);
equal(
  flat({ ...OWNER, settings_sections: [...OWNER.settings_sections, "templates"] }).includes("Templates"),
  true,
  "a hosted board that lists templates shows the sidebar entry",
);
const BILLING_OWNER = {
  ...OWNER,
  hosted: true,
  platform_account_configured: true,
  signed_in: true,
  session: {
    id: "billing-owner", account_id: "acct_owner", origin: "public", created: 1, idle_expires_at: 2, expires_at: 2,
    last_used: 1, user_agent: "fixture",
    user: { sub: "usr_owner", role: "operator", name: "Owner", email: "owner@example.test", handle: "owner" },
  } satisfies NonNullable<Meta["session"]>,
};
const BILLING_MEMBER = {
  ...BILLING_OWNER,
  ...MEMBER,
  session: {
    ...BILLING_OWNER.session,
    user: { ...BILLING_OWNER.session.user, sub: "usr_member", role: "member" as const },
  },
};
equal(flat(BILLING_OWNER).includes("Billing"), true, "the configured hosted owner sees Billing");
equal(settingsRedirect("account", BILLING_OWNER), null, "the owner can open a Billing link");
equal(flat(BILLING_MEMBER).includes("Billing"), false, "members do not see Billing despite the configured capability");
equal(settingsRedirect("account", BILLING_MEMBER), "members", "a member's Billing link returns to Settings");
equal(settingsRedirect("account", { ...BILLING_MEMBER, settings_sections: undefined }), "models", "an older hosted member cannot use a Billing link");
equal(flat({ ...BILLING_OWNER, session: null }).includes("Billing"), false, "unconfirmed ownership stays hidden");
equal(flat({ ...BILLING_OWNER, signed_in: false }).includes("Billing"), false, "a signed-out owner cannot see Billing");
equal(flat({
  ...BILLING_OWNER,
  session: { ...BILLING_OWNER.session, user: { ...BILLING_OWNER.session.user, sub: "operator-token:ops_test" } },
}).includes("Billing"), false, "platform operator credentials are not workspace ownership");
equal(flat({ platform_account_configured: true }).includes("Billing"), true, "the local connected account stays available");
equal(settingsLabel("account"), "Billing", "the crumb names Billing");
equal(settingsRedirect("models", OWNER), "members", "bare /settings opens on a working page");
equal(settingsRedirect("permissions", OWNER), "members", "a direct link to a hidden section moves");
equal(settingsRedirect("connections", OWNER), null, "a served section stays");
equal(settingsRedirect("email", OWNER), null, "hosted email remains available when advertised");
equal(settingsRedirect("email", MEMBER), "members", "hosted email stays unavailable when not advertised");
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
  ["Preferences", "Members", "Documents", "Connections", "General", "Status"],
  "a board that serves the lifecycle offers Documents after Members",
);
equal(flat(OWNER).includes("Documents"), false, "a board that does not name documents hides it");
equal(flat(null).includes("Documents"), false, "a native board has no Documents section");
equal(settingsRedirect("documents", DOCUMENTS), null, "a listed documents section stays");
equal(settingsRedirect("documents", OWNER), "members", "an unlisted documents section moves away");
equal(settingsRedirect("documents", {}), "models", "a native board does not expose hosted documents");
equal(settingsRedirect("documents", { settings_sections: ["preferences"] }), "preferences", "a board without the capability refuses the direct link");
equal(settingsLabel("documents"), "Documents", "the crumb names the section");

// CAD-2578: Settings → You → Security is hosted only, offered below Preferences when listed.
const SECURITY = { settings_sections: ["preferences", "security", "general", "members", "connections", "email", "status"] };
const MEMBER_SECURITY = { settings_sections: ["preferences", "security", "members"] };
equal(
  settingsNavGroups(SECURITY).find((g) => g.label === "You")?.items.map((i) => i.label),
  ["Preferences", "Security"],
  "security sits under You immediately below Preferences when listed",
);
equal(
  flat(MEMBER_SECURITY),
  ["Preferences", "Security", "Members"],
  "a hosted member sees Security when advertised",
);
equal(flat(OWNER).includes("Security"), false, "a board that does not name security hides it");
equal(flat(null).includes("Security"), false, "a native board has no Security section");
equal(settingsRedirect("security", SECURITY), null, "a listed security section stays");
equal(settingsRedirect("security", OWNER), "members", "an unlisted security section moves away");
equal(settingsRedirect("security", {}), "models", "a native board does not expose hosted security");
equal(settingsRedirect("security", { settings_sections: ["preferences"] }), "preferences", "a board without security refuses the direct link");
equal(settingsLabel("security"), "Security", "the crumb names Security");

// CAD-2746: Settings → System → Daemons is hosted owner-only, offered when listed.
const OWNER_DAEMONS = { settings_sections: ["preferences", "general", "members", "connections", "email", "status", "daemons"] };
equal(
  settingsNavGroups(OWNER_DAEMONS).find((g) => g.label === "System")?.items.map((i) => i.label),
  ["Daemons", "Status"],
  "daemons sits under System above Status for owners",
);
equal(flat(OWNER).includes("Daemons"), false, "a board without daemons in settings_sections hides it");
equal(flat(MEMBER).includes("Daemons"), false, "members do not see Daemons");
equal(flat(null).includes("Daemons"), false, "a native board has no Daemons section");
equal(settingsRedirect("daemons", OWNER_DAEMONS), null, "a listed daemons section stays");
equal(settingsRedirect("daemons", MEMBER), "members", "a member link to daemons redirects away");
equal(settingsRedirect("daemons", {}), "models", "a native board redirects away from daemons");
equal(settingsLabel("daemons"), "Daemons", "the crumb names Daemons");
