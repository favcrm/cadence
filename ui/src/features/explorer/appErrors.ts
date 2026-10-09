/**
 * User copy for the Apps screens (CAD-1209). The daemon's refusals are
 * written for operators and logs: they carry backticks, command names and
 * internal ids. Nothing here shows that text. A known refusal maps to a
 * sentence a person can act on; anything that still looks like daemon text
 * (backticks, a `cadence ...` command) falls back to a plain sentence.
 */
const SIGN_IN = /operator'?s session|ui login|carries none|not signed in|sign in|unauthori[sz]ed|\b40[13]\b/i;
const UNCHANGED_UPGRADE = /^upgrade bundle is unchanged$/i;

export function isUnchangedUpgradeError(cause: unknown): boolean {
  const text = cause instanceof Error ? cause.message : typeof cause === "string" ? cause : "";
  return UNCHANGED_UPGRADE.test(text);
}

const PATTERNS: { test: RegExp; copy: string }[] = [
  {
    test: UNCHANGED_UPGRADE,
    copy: "Up to date — this package is the version you have.",
  },
  {
    test: SIGN_IN,
    copy: "You're not signed in as an admin. Sign in to see and manage your apps.",
  },
  {
    test: /digest|changed after|install-check|no longer matches|differs from the checked/i,
    copy: "This app changed after you checked it. Nothing was installed. Check it again to review the current version.",
  },
  {
    test: /restore window/i,
    copy: "The 30-day restore window has closed, so this app can only stay removed.",
  },
  {
    test: /already has workspace installation|already installed|replacement\/upgrade/i,
    copy: "This app is already in your workspace. If you removed it, restore it from Apps.",
  },
  {
    test: /installation is removed|is removed/i,
    copy: "This app is removed. Restore it first.",
  },
];

/** Does this still read like daemon text a person should not see? */
function looksInternal(text: string): boolean {
  return /`|\bcadence [a-z]|--[a-z]|\bpm\b|\.sock\b|\.apps\/|\.ya?ml\b|\/tmp\/|[0-9a-f]{20,}/i.test(text);
}

/** Copy for a failed call. `fallback` is the plain sentence for "something else went wrong". */
export function appErrorCopy(cause: unknown, fallback: string): string {
  const text = cause instanceof Error ? cause.message : typeof cause === "string" ? cause : "";
  for (const { test, copy } of PATTERNS) if (test.test(text)) return copy;
  if (text === "" || looksInternal(text)) return fallback;
  return text;
}

/** A signed-in-but-unverified viewer: workspace apps are hidden from this browser. */
export const UNVERIFIED_APPS_COPY =
  "Workspace apps are hidden because this browser isn't signed in as a team member. Sign in to see them.";

/** Copy for a failed LOAD of a list or page: a sign-in refusal explains the hidden apps. */
export function appLoadErrorCopy(cause: unknown, fallback: string): string {
  const text = cause instanceof Error ? cause.message : typeof cause === "string" ? cause : "";
  return SIGN_IN.test(text) ? UNVERIFIED_APPS_COPY : appErrorCopy(cause, fallback);
}
