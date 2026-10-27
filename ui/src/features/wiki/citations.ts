import { NO_FILTERS } from "../../lib/filters";
import { locationHref, type Route } from "../../lib/router";
import { collectionRoot } from "./knowledge";
import type { CorrectionCitation } from "./knowledge";

const REVISION = /^(?:fnv1a:[0-9a-f]{16}|[0-9a-f]{40})$/;
const PATH = /^(?:global|projects)(?:\/[A-Za-z0-9._~-]+)+$/;

export function safeWikiPath(path: string): string | null {
  if (!PATH.test(path)) return null;
  if (path.split("/").some((part) => part === "." || part === "..")) return null;
  return path;
}

export function safeRevision(revision: string): string | null {
  const value = revision.trim();
  if (!REVISION.test(value)) return null;
  return value.startsWith("fnv1a:") ? value.toLowerCase() : value.toLowerCase();
}

/** Real router href for one historical wiki path. `at` is always present. */
export function wikiRevisionHref(path: string, pin: { at: string; section?: string; page?: number }): string {
  const route: Route = {
    screen: "wiki",
    mode: "browse",
    path,
    query: null,
    at: pin.at,
    ...(pin.section ? { section: pin.section } : {}),
    ...(pin.page !== undefined ? { page: pin.page } : {}),
  };
  return locationHref({ route, project: "all", view: "list", openId: null, filters: NO_FILTERS });
}

export function isWikiRevisionHref(href: string): boolean {
  if (!href.startsWith("/wiki/")) return false;
  const query = href.indexOf("?");
  if (query < 0) return false;
  const params = new URLSearchParams(href.slice(query + 1));
  const at = params.get("at");
  if (!at || !safeRevision(at)) return false;
  const path = href.slice("/wiki/".length, query).split("/").map((part) => {
    try { return decodeURIComponent(part); } catch { return ""; }
  }).join("/");
  return safeWikiPath(path) !== null;
}

interface FoundCitation {
  start: number;
  end: number;
  citation: CorrectionCitation;
  label: string;
}

const CITE = /((?:global|projects)\/[A-Za-z0-9._~/-]+)\s+@\s+(fnv1a:[0-9a-fA-F]{16}|[0-9a-fA-F]{40})/g;

function cleanSection(value: string | undefined): string | undefined {
  const cut = value?.split(/\.\s/)[0]?.trim().replace(/[.,;:]+$/, "").trim();
  if (!cut) return undefined;
  return cut.length > 120 ? cut.slice(0, 120) : cut;
}

/** Citations written in the visible answer. Unsafe paths and revisions are omitted. */
export function citationsIn(text: string): FoundCitation[] {
  const found: FoundCitation[] = [];
  for (const match of text.matchAll(CITE)) {
    const path = safeWikiPath(match[1].replace(/\/+$/, ""));
    const revision = safeRevision(match[2]);
    if (match.index === undefined || !path || !revision) continue;
    let end = match.index + match[0].length;
    const rest = text.slice(end);
    const sectionMatch = /^[ \t]+#([^\n]+)/.exec(rest);
    const section = cleanSection(sectionMatch?.[1].replace(/\s+page\s+\d{1,6}\s*$/, ""));
    if (sectionMatch) end += sectionMatch[0].length;
    const pageMatch = /^[ \t]+page[ \t]+(\d{1,6})/.exec(text.slice(end));
    const page = pageMatch ? Number(pageMatch[1]) : undefined;
    if (pageMatch) end += pageMatch[0].length;
    if (page !== undefined && (page < 1 || page > 999999)) continue;
    found.push({
      start: match.index,
      end,
      label: text.slice(match.index, end).trim(),
      citation: { path, revision, ...(section ? { section } : {}), ...(page ? { page } : {}) },
    });
  }
  return found;
}

export function sharedCollectionCitations(text: string): { root: string; citations: CorrectionCitation[] } | { error: string } {
  const citations = citationsIn(text).map((item) => item.citation);
  if (citations.length === 0) return { error: "The visible answer cites no shared-collection revision." };
  let root: string | null = null;
  for (const cite of citations) {
    const collection = collectionRoot(cite.path);
    if (!collection || !(cite.path === collection || cite.path.startsWith(`${collection}/`))) {
      return { error: "A cited path is outside a shared collection." };
    }
    if (root && root !== collection) return { error: "Cited sources belong to more than one collection." };
    root = collection;
  }
  return root ? { root, citations } : { error: "A cited path is outside a shared collection." };
}

export function citationSpans(text: string): FoundCitation[] {
  return citationsIn(text);
}
