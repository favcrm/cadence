import { parseLoginNonce, writeBlock, READ_ONLY_REASON } from "../src/features/auth/gate";
import type { Meta } from "../src/lib/types";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

const base: Meta = {
  read_only: false,
  actor: "operator (ui)",
  tailnet_url: null,
  version: "0",
  build_commit: "x",
  build_time: "t",
};

// CAD-313: writes are the operator's only with a session.
equal(writeBlock(null), null, "meta not loaded yet");
equal(writeBlock({ ...base, signed_in: true }), null, "signed in");
equal(writeBlock({ ...base, read_only: true, signed_in: true }), READ_ONLY_REASON, "read-only wins");
const out = writeBlock({ ...base, signed_in: false, login_hint: "cadence ui login" }) ?? "";
equal(out.includes("Sign in with cadence ui login"), true, "how to sign in");
equal(out.includes("`"), false, "plain text, no markdown backticks");
const ts = writeBlock({ ...base, signed_in: false, login_hint: "cadence ui login --tailnet" }) ?? "";
equal(ts.includes("--tailnet"), true, "the tailnet hint");
equal(writeBlock(base), null, "an older server without sessions");

// The fragment nonce: 64 lowercase hex, nothing else.
const nonce = "ab".repeat(32);
equal(parseLoginNonce(`#n=${nonce}`), nonce, "a link");
equal(parseLoginNonce(`#n=${nonce.toUpperCase()}`), null, "uppercase");
equal(parseLoginNonce(`#n=${nonce}&x=1`), null, "trailing junk");
equal(parseLoginNonce("#n="), null, "empty");
equal(parseLoginNonce(""), null, "no fragment");
console.log("write gate checks passed");

// A cookie but no key for this tab: say that each tab signs in.
const tab = writeBlock({ ...base, signed_in: false, tab_signed_out: true, login_hint: "cadence ui login" }) ?? "";
equal(tab.includes("each tab signs in"), true, "new-tab reason");
