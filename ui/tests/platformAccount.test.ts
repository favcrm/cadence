import { displayAmount, safeManageUrl } from "../src/features/settings/platformAccount";

const assert = { equal(actual: unknown, expected: unknown, label = "platform account") {
  if (actual !== expected) throw new Error(`${label}: expected ${String(expected)}, got ${String(actual)}`);
} };

assert.equal(displayAmount("0.000000"), "0");
assert.equal(displayAmount("1234567890.123400"), "1234567890.1234");
assert.equal(displayAmount("-0.000001"), "-0.000001");
assert.equal(safeManageUrl("https://app-v2.agenticos.hk/account?company=acme"), "https://app-v2.agenticos.hk/account?company=acme");
for (const url of [
  "javascript:alert(1)",
  "https://evil.test/account?company=acme",
  "https://app-v2.agenticos.hk.evil.test/account?company=acme",
  "https://user@app-v2.agenticos.hk/account?company=acme",
  "https://app-v2.agenticos.hk/account?company=acme&company=other",
  "https://app-v2.agenticos.hk/account?company=acme#token",
  "https://app-v2.agenticos.hk/settings/billing?workspace=ws_acme",
]) assert.equal(safeManageUrl(url), null, url);
