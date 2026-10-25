/**
 * CAD-1806: the access split App resolves from the probe (CAD-1193).
 * Anonymous reads are a resolved non-operator with no waking banner; a
 * signed-in session whose operator proof is missing is "unavailable"
 * and shows the banner.
 */
import { metaAccess, wakingBanner } from "../src/features/auth/access";
import type { Meta } from "../src/lib/types";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

const base: Meta = {
  anonymous_reads: false,
  device_login: false,
  hosted: false,
  login_hint: "cadence ui login",
  platform_account_configured: false,
  tab_signed_out: false,
  read_only: false,
  actor: "operator (ui)",
  tailnet_url: null,
  version: "0",
  build_commit: "x",
  build_time: "t",
};

const anon = metaAccess({ ...base, signed_in: false, anonymous_reads: true });
equal(anon.operator, false, "anonymous reader resolves as non-operator");
equal(anon.metaUnavailable, false, "anonymous reader is not unavailable");
equal(wakingBanner(anon), false, "anonymous reader sees no waking banner");

const unknown = metaAccess({ ...base, signed_in: true, operator: null });
equal(unknown.operator, null, "signed-in with operator null stays unknown");
equal(unknown.access, "unavailable", "signed-in with operator null is unavailable");
equal(wakingBanner(unknown), true, "signed-in with operator null shows the banner");

const missing = metaAccess({ ...base, signed_in: true });
equal(missing.access, "unavailable", "operator omitted for a signed-in session is unavailable");

const operator = metaAccess({ ...base, signed_in: true, operator: true });
equal(operator.operator, true, "operator");
equal(wakingBanner(operator), false, "operator sees no banner");

console.log("accessSplit.test.ts ok");
