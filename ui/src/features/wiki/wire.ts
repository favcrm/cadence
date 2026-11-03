import type { WikiHistoryResponse, WikiKind, WikiListing, WikiPage, WikiSearchResponse } from "./api";
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
