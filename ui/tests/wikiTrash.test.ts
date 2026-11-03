import { trashItemsQuery, trashQuery, type TrashEntry, type TrashLanding } from "../src/features/wiki/api";
import { confirmedDeleteLanding, confirmedRestoreDestination, destinationError, mergeEntries, mergeLandings, restoreBlock } from "../src/features/wiki/trash";
import { normalizeTrashItems, normalizeTrashLandings, safeTrashFrom } from "../src/features/wiki/wire";
import { matchRoute, routePath } from "../src/lib/router";

/**
 * CAD-2018: the Trash screen's decisions, not its copy. The landing
 * selector, the nullable origin, the local destination pre-flight, the
 * restore block and pagination order are the rules a mistake would turn
 * into a wrong write.
 */

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

function throws(run: () => unknown, what: string): void {
  try {
    run();
  } catch {
    return;
  }
  throw new Error(`${what}: expected a refusal`);
}

interface WireLanding {
  landing_id: string;
  from: string | null;
  path: string | null;
  kind: string | null;
  deleted_at: number | null;
  actor: string | null;
  item_count: number | null;
  size: number | null;
  restorable: boolean;
  reason: string | null;
}

function wireLanding(over: Partial<WireLanding> = {}): WireLanding {
  return {
    landing_id: "1793664012847-9f3ac1de",
    from: ".trash/1793664012847-9f3ac1de/projects/cadence/design/tokens.md",
    path: "projects/cadence/design/tokens.md",
    kind: "text",
    deleted_at: 1793664012847,
    actor: "operator",
    item_count: 1,
    size: 5324,
    restorable: true,
    reason: null,
    ...over,
  };
}

interface WireEntry {
  path: string;
  kind: string;
  size: number | null;
}

const wireEntry = (path: string, kind = "text", size: number | null = 12): WireEntry => ({ path, kind, size });

// --- the route: one screen, no selector in the URL -----------------------
equal(matchRoute("/wiki/trash"), { screen: "wiki", mode: "trash", path: null, query: null }, "the Trash screen");
equal(routePath({ screen: "wiki", mode: "trash", path: null, query: null }), "/wiki/trash", "the Trash href");
// Only an exact first segment reserves: a page named trash.md stays a page.
equal(
  matchRoute("/wiki/trash.md"),
  { screen: "wiki", mode: "browse", path: "trash.md", query: null },
  "trash.md is a page, not the Trash screen",
);

// --- the requests: exact selector, board-owned paging --------------------
equal(trashQuery(), "/api/wiki/trash?limit=50", "the default listing request");
equal(trashQuery("cursor/with+chars"), "/api/wiki/trash?limit=50&cursor=cursor%2Fwith%2Bchars", "an opaque cursor stays encoded");
const spacedSource = ".trash/1793664012847-9f3ac1de/global/a b+c.md";
const rawSource = trashItemsQuery(spacedSource, "next").split("?")[1]!.split("&").find((part) => part.startsWith("from="))!.slice(5);
equal(decodeURIComponent(rawSource), spacedSource, "native percent-only decoding preserves spaces and literal pluses");

// --- the landing listing: null origin is real, foreign origin is not -----
const listed = normalizeTrashLandings({
  landings: [
    wireLanding(),
    wireLanding({
      landing_id: "1793598009220-a71d40e5",
      from: null,
      path: null,
      kind: null,
      deleted_at: null,
      actor: null,
      item_count: null,
      size: null,
      restorable: true,
      reason: "origin_unverified",
    }),
  ],
  next_cursor: "page-2",
});
equal(listed.landings[0]?.kind, "page", "a text landing is a page");
equal(listed.landings[0]?.from, ".trash/1793664012847-9f3ac1de/projects/cadence/design/tokens.md", "the exact selector survives");
equal(listed.landings[1]?.kind, null, "an unprovable original kind stays unknown");
equal(listed.landings[1]?.from, null, "an unprovable origin stays null");
equal(listed.landings[1]?.path, null, "an unprovable original path stays null");
equal(listed.landings[1]?.restorable, false, "a landing with no selector is never restorable");
equal(listed.landings[1]?.item_count, null, "an unknown count is not invented");
equal(
  normalizeTrashLandings({ landings: [wireLanding({ deleted_at: 10_000_000_000_000_000 })] }).landings[0]?.deleted_at,
  null,
  "an out-of-range timestamp is shown as unknown instead of crashing the page",
);
equal(listed.next_cursor, "page-2", "the board's cursor is kept");
equal(normalizeTrashLandings({ landings: [] }).next_cursor, null, "no cursor is null, not empty");

throws(() => normalizeTrashLandings({ landings: [wireLanding({ from: "global/note.md" })] }), "a non-trash selector");
throws(() => normalizeTrashLandings({ landings: [wireLanding({ from: ".trash/../etc/passwd" })] }), "a traversal selector");
throws(() => normalizeTrashLandings({ landings: [wireLanding({ landing_id: "" })] }), "a landing with no id");
throws(() => normalizeTrashLandings({ landings: [wireLanding({ landing_id: "other" })] }), "a selector for a different landing");
throws(
  () => normalizeTrashLandings({ landings: [], next_cursor: 4 as unknown as string }),
  "an invalid cursor cannot masquerade as the last page",
);
throws(() => normalizeTrashLandings({ landings: [wireLanding({ kind: "mystery" })] }), "an unknown entry kind");
throws(() => normalizeTrashLandings({ landings: [wireLanding({ kind: null })] }), "a selector without a known original kind");
throws(() => normalizeTrashLandings({ landings: [wireLanding({ restorable: undefined })] }), "a row with no restorability answer");
equal(
  normalizeTrashLandings({ landings: [wireLanding({ reason: "custody_unverified" })] }).landings[0]?.restorable,
  false,
  "an inconsistent custody refusal cannot enable restore",
);
equal(
  normalizeTrashLandings({ landings: [wireLanding({ reason: undefined })] }).landings[0]?.restorable,
  false,
  "an absent refusal state cannot enable restore",
);

equal(safeTrashFrom(".trash/1-a/global/x.md"), true, "a plain landing selector");
equal(safeTrashFrom(".trash//global/x.md"), false, "an empty landing segment");
equal(safeTrashFrom("global/x.md"), false, "a logical path is not a selector");

// --- one landing's paths: the answer must name that landing --------------
const items = normalizeTrashItems({
  from: ".trash/1793664012847-9f3ac1de/projects/cadence/design/tokens.md",
  entries: [wireEntry("tokens.md"), wireEntry("board.png", "blob", null), wireEntry("notes", "dir", 0)],
  next_cursor: null,
}, ".trash/1793664012847-9f3ac1de/projects/cadence/design/tokens.md");
equal(items.entries.map((entry) => entry.kind), ["page", "file", "dir"], "entry kinds map");
equal(items.entries[1]?.size, null, "an unprovable size stays null");
equal(items.entries[2]?.size, 0, "a proven zero size is kept");

throws(() => normalizeTrashItems({
  from: ".trash/other/x",
  entries: [wireEntry("a.md")],
}, ".trash/1793664012847-9f3ac1de/x"), "a foreign landing answer");

// --- the local destination pre-flight ------------------------------------
equal(destinationError("") !== null, true, "an empty destination is refused");
equal(destinationError("  ") !== null, true, "a blank destination is refused");
equal(destinationError("/global/x.md") !== null, true, "a leading slash is refused");
equal(destinationError("global/x.md/") !== null, true, "a trailing slash is refused");
equal(destinationError("global/a\\b.md") !== null, true, "a backslash is refused locally");
// The original destination is not predicted as a collision: the board
// answers 409, and choosing another path must stay possible.
equal(destinationError("projects/cadence/design/tokens.md"), null, "the original path is offered, not refused");
equal(destinationError("global/restored.md"), null, "another destination is offered");

// --- restore is blocked only for a reason the screen can state -----------
equal(restoreBlock({ restorable: true, reason: null, from: ".trash/1-a/global/x.md" }), null, "a restorable landing is not blocked");
const custody = restoreBlock({ restorable: false, reason: "custody_unverified", from: ".trash/1-a/global/x.md" });
equal(custody?.code, "custody_unverified", "the custody refusal keeps the board's code");
equal(Boolean(custody?.text), true, "the custody refusal explains itself");
const origin = restoreBlock({ restorable: true, reason: null, from: null });
equal(origin?.code, "origin_unverified", "no selector blocks as an unknown origin");
equal(restoreBlock({ restorable: false, reason: "ambiguous_landing", from: ".trash/1-a/global/x.md" })?.code, "ambiguous_landing", "another board code is passed through");
equal(restoreBlock({ restorable: false, reason: "evidence_truncated", from: null })?.code, "evidence_truncated", "incomplete history is not presented as missing history");
equal(restoreBlock({ restorable: false, reason: null, from: ".trash/1-a/global/x.md" })?.code, null, "no code is invented when the board gives none");
equal(restoreBlock({ restorable: true, reason: "custody_unverified", from: ".trash/1-a/global/x.md" })?.code, "custody_unverified", "a contradictory refusal stays blocked");

const from = ".trash/1-a/global/x.md";
const to = "global/x.md";
equal(confirmedRestoreDestination({ from, path: to, restored: true }, from, to), to, "hosted receipt confirms the move");
equal(confirmedRestoreDestination({ from, to, moved: true }, from, to), to, "native receipt confirms the move");
equal(confirmedRestoreDestination({ from, path: to }, from, to), null, "a path without a success receipt is uncertain");
equal(confirmedRestoreDestination({ from: ".trash/other/global/x.md", path: to, restored: true }, from, to), null, "a different source cannot confirm the move");
equal(confirmedRestoreDestination({ from, to: "global/other.md", moved: true }, from, to), null, "a different destination cannot confirm the move");
equal(confirmedDeleteLanding({ path: to, trash: from }, to), from, "a hosted delete receipt confirms the landing");
equal(confirmedDeleteLanding({ path: to, trash: `${from}.blob` }, to), `${from}.blob`, "a native blob pointer receipt confirms the landing");
equal(confirmedDeleteLanding({ path: to, trash: ".trash/1-a/global/other.md" }, to), null, "a different landing does not confirm deletion");
equal(confirmedDeleteLanding({ path: to }, to), null, "a missing landing does not confirm deletion");

// --- pagination keeps the board's order and never repeats a row ----------
const indexedLanding = (id: string) => wireLanding({
  landing_id: id,
  from: `.trash/${id}/global/test.md`,
  path: "global/test.md",
});
const first: TrashLanding[] = normalizeTrashLandings({ landings: [indexedLanding("a")] }).landings;
const second: TrashLanding[] = normalizeTrashLandings({
  landings: [indexedLanding("a"), indexedLanding("b"), indexedLanding("c")],
}).landings;
equal(mergeLandings(first, second).map((landing) => landing.landing_id), ["a", "b", "c"], "pages append without duplicates or reordering");

const entriesA: TrashEntry[] = [{ path: "a.md", kind: "page", size: 1 }];
const entriesB: TrashEntry[] = [{ path: "a.md", kind: "page", size: 1 }, { path: "b.md", kind: "page", size: 2 }];
equal(mergeEntries(entriesA, entriesB).map((entry) => entry.path), ["a.md", "b.md"], "paths append once in order");
equal(
  mergeEntries([{ path: "a", kind: "dir", size: 0 }], [{ path: "a", kind: "page", size: 1 }]).map((entry) => entry.kind),
  ["dir", "page"],
  "distinct kinds at a legacy overlapping path remain visible",
);

console.log("wiki trash checks passed");
