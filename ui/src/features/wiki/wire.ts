import type { WikiHistoryResponse, WikiKind, WikiListing, WikiPage, WikiSearchResponse } from "./api";
import { baseName } from "./paths";

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

export function normalizeSearch(value: WikiSearchResponse & {
  matches?: { path: string; line: number; text: string }[];
}): WikiSearchResponse {
  return value.matches ? {
    hits: value.matches.map((match) => ({ path: match.path, name: baseName(match.path), kind: "page", snippet: match.text, line: match.line })),
  } : value;
}

export function normalizeHistory(value: WikiHistoryResponse & {
  commits?: { sha: string; at: number; subject: string; actor: string }[];
}): WikiHistoryResponse {
  return value.commits ? {
    path: value.path,
    canCompare: false, // The current HTTP surface exposes a log, not diffs or restores.
    entries: value.commits.map((commit) => ({ rev: commit.sha, at: new Date(commit.at * 1000).toISOString(), author: commit.actor, summary: commit.subject })),
  } : value;
}
