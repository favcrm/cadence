import { ApiError } from "../src/lib/api";
import { friendlyAudienceError } from "../src/features/app-shell/segmentGrammar";
import { friendlySendError } from "../src/features/app-shell/sendClient";
import { friendlyError } from "../src/features/app-shell/customerProfile";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { ErrorNotice } from "../src/features/app-shell/shared/States";
import { translateUiText } from "../src/lib/locale";
import { LocaleTextProvider } from "../src/lib/localeText";
import {
  CONNECTION_UNAVAILABLE_TEXT,
  SESSION_EXPIRED_TEXT,
  hostErrorText,
  isGenericRefusal,
} from "../src/features/app-shell/shared/hostErrors";

/** CAD-1009: the one error-to-message layer every CRM list shares. */
function equal(actual: unknown, expected: unknown, why: string): void {
  if (actual !== expected) throw new Error(`${why}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
}

const fns = [friendlyAudienceError, friendlySendError, friendlyError];
for (const fn of fns) {
  // A transport error can also be an old cross-origin sign-in redirect; it
  // does not prove this browser's session ended.
  equal(fn(new TypeError("Failed to fetch")), CONNECTION_UNAVAILABLE_TEXT, "a network error is not a confirmed sign-out");
  equal(fn({ type: "opaqueredirect" }), CONNECTION_UNAVAILABLE_TEXT, "an opaque redirect is not a confirmed sign-out");
  equal(fn(new ApiError("anything", 401)), SESSION_EXPIRED_TEXT, "a 401 reads as an expired session");
  // An object coerced into an ApiError message is never shown.
  const coerced = new ApiError({ code: "x" } as unknown as string, 502);
  equal(fn(coerced).includes("[object"), false, "an object message is never stringified");
  // Real server messages stay verbatim.
  equal(fn(new ApiError("subject is required", 400)).includes("subject is required"), true, "a server refusal stays verbatim");
  equal(fn(undefined).includes("[object"), false, "undefined never stringifies");
}

for (const locale of ["en", "zh-TW"] as const) {
  const t = (text: string) => translateUiText(text, locale);
  const notice = (text: string, onRetry?: () => void) => renderToStaticMarkup(
    createElement(LocaleTextProvider, { value: t },
      createElement(ErrorNotice, { onRetry, children: t(text) }),
    ),
  );
  const expired = notice(SESSION_EXPIRED_TEXT);
  equal(expired.includes("<button"), true, `${locale}: a confirmed expiry offers reload`);
  equal(expired.includes(t("Reload")), true, `${locale}: the reload action is translated`);
  const unavailable = notice(CONNECTION_UNAVAILABLE_TEXT);
  equal(unavailable.includes("<button"), false, `${locale}: an ambiguous failure does not offer a destructive reload`);
  const retryable = notice(CONNECTION_UNAVAILABLE_TEXT, () => {});
  equal(retryable.includes("<button"), true, `${locale}: a provided retry remains available`);
  equal(retryable.includes(t("Retry")), true, `${locale}: the retry action is translated`);
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
