import { previewKind } from "./preview";

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
  /**
   * The host's own kind when it prints one (`page` or `file`); `null` when it
   * does not. The native index prints no kind, so it is inferred from the name
   * there — never assumed to be a page.
   */
  kind?: "page" | "file" | null;
  mime?: string | null;
  snippet: string;
  line?: number;
  /** The indexed text's revision, when the board printed it. */
  rev?: string;
  /** Native: the page the indexed text came from. Hosted: `page`, `extracted` or `name`. */
  source?: string;
}

/** The chips a file can claim by its own name or MIME. */
function fileChip(name: string, mime?: string | null): Exclude<TypeFilter, "all"> | null {
  switch (previewKind(name, mime)) {
    case "image":
      return "images";
    case "video":
      return "video";
    case "pdf":
      return "pdf";
    default:
      return null;
  }
}

/**
 * Which chip a hit belongs to, or `null` when it has no chip of its own (a
 * zip, an archive, any other attachment) and so appears only under All. A
 * page is never counted as a file and an attachment is never counted as a
 * page: the hosted board says which it is, and the native board's non-pointer
 * files are all text pages.
 */
export function hitKind(hit: SearchHit): Exclude<TypeFilter, "all"> | null {
  if (hit.kind === "page") return "pages";
  if (hit.kind === "file" || hit.mime) return fileChip(hit.name, hit.mime);
  return fileChip(hit.name, hit.mime) ?? "pages";
}

export function filterHits(hits: SearchHit[], filter: TypeFilter): SearchHit[] {
  return filter === "all" ? hits : hits.filter((hit) => hitKind(hit) === filter);
}

export interface SearchResult {
  path: string;
  name: string;
  matches: SearchHit[];
}

/** One result per file, preserving the server's file and excerpt order. */
export function groupHits(hits: SearchHit[]): SearchResult[] {
  const files = new Map<string, SearchResult>();
  for (const hit of hits) {
    let file = files.get(hit.path);
    if (!file) {
      file = { path: hit.path, name: hit.name, matches: [] };
      files.set(hit.path, file);
    }
    file.matches.push(hit);
  }
  return [...files.values()];
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
