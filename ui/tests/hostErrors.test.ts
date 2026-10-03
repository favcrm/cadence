import { ApiError } from "../src/lib/api";
import { friendlyAudienceError } from "../src/features/app-shell/segmentGrammar";
import { friendlyCampaignError } from "../src/features/app-shell/campaignGrammar";
import { friendlySendError } from "../src/features/app-shell/sendClient";
import { friendlyError } from "../src/features/app-shell/customerProfile";
import {
  SESSION_EXPIRED_TEXT,
  hostErrorText,
  isGenericRefusal,
} from "../src/features/app-shell/shared/hostErrors";

/** CAD-1009: the one error-to-message layer every CRM list shares. */
function equal(actual: unknown, expected: unknown, why: string): void {
  if (actual !== expected) throw new Error(`${why}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
}

const fns = [friendlyAudienceError, friendlyCampaignError, friendlySendError, friendlyError];
for (const fn of fns) {
  // Expired hosted session: fetch throws a TypeError or a bare object, or a 401.
  equal(fn(new TypeError("Failed to fetch")), SESSION_EXPIRED_TEXT, "a network/redirect TypeError reads as an expired session");
  equal(fn({ type: "opaqueredirect" }), SESSION_EXPIRED_TEXT, "a thrown bare object reads as an expired session");
  equal(fn(new ApiError("anything", 401)), SESSION_EXPIRED_TEXT, "a 401 reads as an expired session");
  // An object coerced into an ApiError message is never shown.
  const coerced = new ApiError({ code: "x" } as unknown as string, 502);
  equal(fn(coerced).includes("[object"), false, "an object message is never stringified");
  // Real server messages stay verbatim.
  equal(fn(new ApiError("subject is required", 400)).includes("subject is required"), true, "a server refusal stays verbatim");
  equal(fn(undefined).includes("[object"), false, "undefined never stringifies");
}

// The daemon's bare management line is dropped, never glued to a message.
const generic = new ApiError("app content management refused or unavailable", 409);
equal(isGenericRefusal(generic), true, "the bare management refusal is generic");
equal(hostErrorText(generic, "fallback"), "fallback", "a bare management refusal falls back");
equal(
  hostErrorText(new ApiError("app content management refused or unavailable: revision is stale", 409), "fallback"),
  "revision is stale",
  "the generic prefix is stripped from a reasoned refusal",
);
equal(isGenericRefusal(new ApiError("subject is required", 400)), false, "a real refusal is not generic");

console.log("host errors checks passed");
