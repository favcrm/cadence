import { describeRefusal, nextDelaySecs, safeVerificationHref, startDelaySecs } from "../src/features/auth/deviceFlow";
import { ApiError } from "../src/lib/api";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

// CAD-777: the approval link is only ever https/http — the complete
// URI wins, and anything else is no link at all.
const display = {
  verification_uri: "https://issuer.example/verify",
  verification_uri_complete: "https://issuer.example/verify?code=ABCD-1234",
};
equal(
  safeVerificationHref(display),
  "https://issuer.example/verify?code=ABCD-1234",
  "complete uri preferred",
);
equal(
  safeVerificationHref({ verification_uri: "https://issuer.example/verify" }),
  "https://issuer.example/verify",
  "falls back to verification_uri",
);
equal(
  safeVerificationHref({ verification_uri: "http://127.0.0.1:9/verify" }),
  "http://127.0.0.1:9/verify",
  "loopback http fixture",
);
for (const bad of [
  "javascript:alert(1)",
  "data:text/html,x",
  "not a url",
  "ftp://issuer.example/verify",
  "//issuer.example/verify",
]) {
  equal(
    safeVerificationHref({ verification_uri: bad, verification_uri_complete: bad }),
    null,
    `refuses ${bad}`,
  );
}
// A bad complete URI does not shadow a good plain one.
equal(
  safeVerificationHref({
    verification_uri: "https://issuer.example/verify",
    verification_uri_complete: "javascript:alert(1)",
  }),
  null,
  "bad complete wins over good plain — no fallback to a second choice",
);

// RFC 8628 pacing: slow_down adds 5 s, pending keeps it, floor is 1 s.
equal(nextDelaySecs(5, "pending"), 5, "pending keeps the interval");
equal(nextDelaySecs(5, "slow_down"), 10, "slow_down adds 5 s");
equal(nextDelaySecs(0, "pending"), 1, "floor of 1 s");
equal(startDelaySecs({ interval: 5 }), 5, "server interval");
equal(startDelaySecs({ interval: 0 }), 1, "floored interval");

// Refusals: the subject refusal passes the server's message through
// (it names the subject id and the flag the operator adds).
const refused = new ApiError(
  "device sign-in refused: subject 'op_9' is not on this board's device-login allowlist",
  403,
  { code: "device_subject_not_allowed" },
);
equal(
  describeRefusal(refused).includes("op_9"),
  true,
  "subject-not-allowed message passes through",
);
equal(
  describeRefusal(new ApiError("x", 502, { code: "device_verification_failed" })),
  "The issuer could not confirm the approval — start again.",
  "verification failure is its own message",
);
equal(describeRefusal(new ApiError("x", 403, { check: "session_from_agent" })).includes("agent"), true, "agent refusal");
equal(describeRefusal(new ApiError("x", 502, { code: "issuer_unavailable" })).includes("issuer"), true, "issuer down");
equal(describeRefusal(new ApiError("x", 429, {})).includes("Too many"), true, "429");
equal(describeRefusal(new Error("boom")).length > 0, true, "generic error");
