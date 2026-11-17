/**
 * CAD-2418: the hosted member history page agenticos-v2 serves must stay one
 * the member chat reader accepts.
 *
 * `tests/fixtures/agenticos/member-history.contract.json` is a byte-identical
 * copy of agenticos-v2's
 * `apps/api/src/tenant-board/assistant/fixtures/member-history.contract.json`,
 * produced there by the real page code over a synthetic transcript. v2's own
 * test fails when its answer changes; this one fails when `memberPage` stops
 * accepting it. That drift shipped once: CAD-2215 tightened the reader while
 * the hosted board still answered numeric cursors, and every hosted member's
 * thread failed to read.
 *
 * Refresh deliberately: v2 regenerates its copy with
 * `UPDATE_MEMBER_HISTORY_FIXTURE=1`, and the same bytes land here in a paired
 * PR. A reader change that refuses the fixture needs the v2 change first.
 */
import { memberPage } from "../src/features/home/memberThread";
import type { MemberAssistHistory } from "../src/lib/api";

declare function require(name: string): {
  readFileSync(path: string, encoding: "utf8"): string;
};
declare const process: { cwd(): string };
const { readFileSync } = require("fs");

const fixture = JSON.parse(
  readFileSync(`${process.cwd()}/../tests/fixtures/agenticos/member-history.contract.json`, "utf8"),
) as Record<string, unknown>;

const PAGES = ["empty_tail", "tail", "before", "forward", "caught_up"];

function refused(page: unknown): string | null {
  try {
    memberPage(page as MemberAssistHistory);
    return null;
  } catch (error) {
    return (error as Error).message;
  }
}

const names = Object.keys(fixture).filter((name) => name !== "synthetic");
if (JSON.stringify(names) !== JSON.stringify(PAGES))
  throw new Error(`fixture pages changed: expected ${PAGES.join(",")}, got ${names.join(",")}`);

for (const name of PAGES) {
  const reason = refused(fixture[name]);
  if (reason !== null) throw new Error(`memberPage refused the v2 "${name}" page: ${reason}`);
}

// Negative control: the numeric page v2 served before #2142 is still refused,
// so this file can fail.
const legacy = {
  conversation: (fixture.tail as { conversation: unknown }).conversation,
  entries: [],
  cursor: 0,
  more_before: false,
};
if (refused(legacy) === null) throw new Error("memberPage accepted a numeric cursor");

console.log("member history contract checks passed");
