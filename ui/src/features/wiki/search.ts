import { extOf, previewKind } from "./preview";

/**
 * Search results' view-model (CAD-581): the type chips, and the snippet
 * split into plain and highlighted parts. The server's snippet is text
 * (CAD-580 returns the matching line); the query's occurrences in it are
 * marked here, so the rendering is the same whatever shape the server
 * eventually returns marks in.
 */

export type TypeFilter = "all" | "pages" | "images" | "video" | "pdf";

export const TYPE_FILTERS: TypeFilter[] = ["all", "pages", "images", "video", "pdf"];

export interface SearchHit {
  path: string;
  name: string;
  kind?: string | null;
  mime?: string | null;
  snippet: string;
}

export function hitKind(hit: SearchHit): Exclude<TypeFilter, "all"> {
  if (hit.kind === "page" || extOf(hit.name) === "md") return "pages";
  switch (previewKind(hit.name, hit.mime)) {
    case "image":
      return "images";
    case "video":
      return "video";
    case "pdf":
      return "pdf";
    default:
      return "pages";
  }
}

export function filterHits(hits: SearchHit[], filter: TypeFilter): SearchHit[] {
  return filter === "all" ? hits : hits.filter((hit) => hitKind(hit) === filter);
}

export interface SnippetPart {
  text: string;
  mark: boolean;
}

/** The snippet as parts, each query occurrence marked (case-insensitive). */
export function snippetParts(snippet: string, query: string): SnippetPart[] {
  const needle = query.trim();
  if (!needle) return snippet ? [{ text: snippet, mark: false }] : [];
  const haystack = snippet.toLowerCase();
  const lower = needle.toLowerCase();
  const parts: SnippetPart[] = [];
  let at = 0;
  for (;;) {
    const found = haystack.indexOf(lower, at);
    if (found === -1) break;
    if (found > at) parts.push({ text: snippet.slice(at, found), mark: false });
    parts.push({ text: snippet.slice(found, found + needle.length), mark: true });
    at = found + needle.length;
  }
  if (at < snippet.length) parts.push({ text: snippet.slice(at), mark: false });
  return parts;
}
