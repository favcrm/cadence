import type {
  TrashEntry,
  TrashItems,
  TrashLanding,
  TrashLandings,
  WikiHistoryResponse,
  WikiKind,
  WikiListing,
  WikiPage,
  WikiSearchResponse,
} from "./api";
import { baseName } from "./paths";
import type { SearchHit } from "./search";

/** Translate the daemon's text/blob names into the UI's page/file model. */
function wikiKind(kind: string): WikiKind {
  if (kind === "text" || kind === "page") return "page";
  if (kind === "blob" || kind === "file") return "file";
  if (kind === "dir") return "dir";
  throw new Error(`Unknown wiki entry kind: ${kind}`);
}

export function normalizeListing(value: Omit<WikiListing, "kind" | "entries"> & {
  kind?: string; entries?: (Omit<NonNullable<WikiListing["entries"]>[number], "kind"> & { kind: string })[];
}): WikiListing {
  return {
    ...value,
    kind: value.kind ? wikiKind(value.kind) : undefined,
    entries: value.entries?.map((entry) => ({ ...entry, kind: wikiKind(entry.kind) })),
  };
}

export function normalizePage(value: Omit<WikiPage, "kind"> & { kind: string }): WikiPage {
  const kind = wikiKind(value.kind);
  if (kind === "dir") throw new Error("This wiki path is a folder");
  return { ...value, kind };
}

/** One search row, as either board prints it. */
interface WireMatch {
  path: string;
  line?: number;
  text: string;
  heading?: string;
  rev?: string;
  /** Native: the page the indexed text came from. Hosted: `page`, `extracted` or `name`. */
  source?: string;
  /** Hosted only: `text` (a page) or `blob` (an attachment). */
  kind?: string;
}

/** A wire row as the pane's hit, preserving each host's search contract. */
function searchHit(match: WireMatch): SearchHit {
  const name = baseName(match.path);
  // Native index matches are always text pages, even after a rename gives
  // one an attachment-like extension. Older hosted replies instead identify
  // indexed attachment text with `source: "extracted"`.
  const kind = match.kind === "text" ? "page"
    : match.kind === "blob" ? "file"
      : match.kind === undefined ? (match.source === "extracted" || match.source === "name" ? "file" : "page")
        : null;
  return {
    path: match.path,
    name,
    kind,
    snippet: match.text,
    ...(match.line !== undefined ? { line: match.line } : {}),
    ...(match.rev ? { rev: match.rev } : {}),
    ...(match.source ? { source: match.source } : {}),
  };
}

export function normalizeSearch(value: WikiSearchResponse & {
  matches?: WireMatch[];
}): WikiSearchResponse {
  if (!value.matches) return value;
  return {
    hits: value.matches.map(searchHit),
    // The fallback disclosure travels with the hits it describes. A native
    // board sends none, which is not the same as "complete" — it simply never
    // falls back, so no note is shown.
    ...(value.partial === true ? { partial: true } : {}),
  };
}

export function normalizeHistory(value: WikiHistoryResponse & {
  commits?: { sha: string; at: number; subject: string; actor: string; can_open?: boolean }[];
}): WikiHistoryResponse {
  return value.commits ? {
    path: value.path,
    // The board's authoritative current revision, when it says one. The first
    // log row is not it: a move or a scope-filtered log can start with a row
    // that is not the page's current content.
    ...(value.rev !== undefined ? { rev: value.rev } : {}),
    canCompare: false, // The current HTTP surface exposes a log, not diffs or restores.
    // A board may declare whether old revisions open (AOS-452); HistoryPane defaults by host.
    canOpen: value.canOpen,
    entries: value.commits.map((commit) => ({
      rev: commit.sha,
      at: new Date(commit.at * 1000).toISOString(),
      author: commit.actor,
      summary: commit.subject,
      // Per-entry availability: a hosted log row whose text was not retained
      // (or whose original scope this caller cannot read) says so. Absent
      // means the host did not say, and the pane falls back to its own rule.
      ...(commit.can_open !== undefined ? { can_open: commit.can_open } : {}),
    })),
  } : value;
}

/* ------------------------------------------------- CAD-2018: trash wire */

type WireTrashLanding = Omit<TrashLanding, "kind"> & { kind: string | null };
type WireTrashEntry = Omit<TrashEntry, "kind"> & { kind: string };

/** A non-negative finite number, or null when the board did not prove one. */
function provenNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : null;
}

function provenTimestamp(value: unknown): number | null {
  const ms = provenNumber(value);
  return ms !== null && Number.isSafeInteger(ms) && ms <= 8_640_000_000_000_000 ? ms : null;
}

function provenString(value: unknown): string | null {
  return typeof value === "string" && value ? value : null;
}

function trashCursor(value: unknown): string | null {
  if (value === null || value === undefined) return null;
  if (typeof value === "string" && value) return value;
  throw new Error("the board returned an invalid trash cursor");
}

/**
 * The landing selector is the only restore source the board accepts, so it
 * is checked before it is ever sent back: `.trash/`, a landing segment and
 * an original path, with no empty or traversal segment. The board still
 * rejects anything foreign; this only keeps a malformed row off the screen.
 */
export function safeTrashFrom(from: string): boolean {
  if (!from.startsWith(".trash/")) return false;
  const parts = from.split("/");
  if (parts.length < 3) return false;
  return parts.every((part) => part !== "" && part !== "." && part !== "..");
}

/**
 * `GET /api/wiki/trash`. Fail closed on a malformed row: a landing with no
 * identity, a selector outside `.trash/` or no restorability answer is not
 * shown at all, rather than rendered with invented values. A null `from`
 * is a real answer — the board cannot prove the origin — so it stays null
 * and is never made restorable here.
 */
export function normalizeTrashLandings(value: {
  landings?: WireTrashLanding[];
  next_cursor?: string | null;
}): TrashLandings {
  if (!Array.isArray(value.landings)) throw new Error("the trash listing had no landings");
  return {
    landings: value.landings.map((landing) => {
      const landingId = typeof landing.landing_id === "string" ? landing.landing_id : "";
      if (!landingId) throw new Error("the trash listing returned a landing without an id");
      const from = typeof landing.from === "string" && landing.from ? landing.from : null;
      if (from !== null && (!safeTrashFrom(from) || from.split("/")[1] !== landingId)) {
        throw new Error("the trash listing returned a foreign landing selector");
      }
      if (landing.kind === null && from !== null) {
        throw new Error("the trash listing returned a selector without a known kind");
      }
      if (typeof landing.restorable !== "boolean") {
        throw new Error("the trash listing did not say whether a landing is restorable");
      }
      const reason = provenString(landing.reason);
      const reasonKnown = landing.reason === null || reason !== null;
      return {
        landing_id: landingId,
        from,
        path: provenString(landing.path),
        kind: landing.kind === null ? null : wikiKind(landing.kind),
        deleted_at: provenTimestamp(landing.deleted_at),
        actor: provenString(landing.actor),
        item_count: provenNumber(landing.item_count),
        size: provenNumber(landing.size),
        // A landing with no provable origin cannot be a restore source, no
        // matter what the row claims.
        restorable: landing.restorable && from !== null && reasonKnown && reason === null,
        reason,
      };
    }),
    next_cursor: trashCursor(value.next_cursor),
  };
}

/**
 * `GET /api/wiki/trash/items`. The answer must name the landing that was
 * asked for — a different `from` is a foreign answer, not a page to append.
 */
export function normalizeTrashItems(value: {
  from?: string;
  entries?: WireTrashEntry[];
  next_cursor?: string | null;
}, requested: string): TrashItems {
  if (value.from !== requested) throw new Error("the board answered a different trash landing");
  if (!Array.isArray(value.entries)) throw new Error("the trash landing had no entries");
  return {
    from: requested,
    entries: value.entries.map((entry) => {
      if (typeof entry.path !== "string" || !entry.path) {
        throw new Error("the trash landing returned an entry without a path");
      }
      return { path: entry.path, kind: wikiKind(entry.kind), size: provenNumber(entry.size) };
    }),
    next_cursor: trashCursor(value.next_cursor),
  };
}
