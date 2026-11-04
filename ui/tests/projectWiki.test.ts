import { contextHref, contextRoute, contextNavigationSearch } from "../src/features/projects/contextRoute";
import { scopedAncestors, withinScope } from "../src/features/wiki/scope";
import { normalizeListing, normalizePage, normalizeSearch, normalizeHistory } from "../src/features/wiki/wire";
import { isMissingWikiFolder, searchQuery, WikiError } from "../src/features/wiki/api";
function equal(a: unknown, b: unknown, what: string) { if (JSON.stringify(a) !== JSON.stringify(b)) throw new Error(what); }
const root = "projects/cadence";
equal(withinScope("projects/cadence-old/a.md", root), false, "similar project names never match");
for (const path of ["global/a.md", `${root}/../other/a.md`, `${root}/%2e%2e/a.md`, `${root}/a\\b.md`]) equal(withinScope(path, root), false, "out-of-scope path refused");
equal(scopedAncestors(`${root}/research`, root), [root, `${root}/research`], "do not fetch global parents of project tree");
equal(contextRoute("cadence", "/projects/cadence/context")?.path, root, "default route stays in project");
equal(contextRoute("cadence", "?file=..%2Fother%2Fa.md"), null, "traversal query rejected");
const route = { screen: "wiki", mode: "edit", path: `${root}/research/my notes.md`, query: null } as const;
const href = contextHref("cadence", route, "?issue=CAD-1");
equal(contextRoute("cadence", href), route, "edit deep link roundtrips");
equal(href.includes("issue=CAD-1"), true, "open issue drawer survives context navigation");
equal(contextHref("cadence", { ...route, mode: "browse", path: null }, "?file=old.md&mode=edit&q=old"), "/projects/cadence/context", "root navigation clears old mode and file");
equal(searchQuery("agent", undefined, root).includes("path=projects%2Fcadence"), true, "search sends project scope to the daemon");
// The first-time empty project Context: only the two answers that really mean
// "this folder does not exist" open it, and nothing else is folded in.
equal(isMissingWikiFolder(new WikiError("no such wiki folder", 404, { code: "not_found" }), root), true, "hosted not_found is an empty project context");
equal(isMissingWikiFolder(new WikiError(`wiki ls '${root}': no such directory`, 400), root), true, "the daemon's missing-directory answer is too");
equal(isMissingWikiFolder(new WikiError(`wiki ls '${root}': no such directory`, 500), root), false, "a server failure is not an empty project");
equal(isMissingWikiFolder(new WikiError("no such directory", 400), root), false, "a message alone is not the daemon's answer");
equal(isMissingWikiFolder(new WikiError("wiki ls 'projects/other': no such directory", 400), root), false, "another folder's answer does not stand in for this one");
equal(isMissingWikiFolder(new WikiError("forbidden", 403, { code: "forbidden" }), root), false, "a refusal stays a refusal");
equal(isMissingWikiFolder(new Error("Failed to fetch"), root), false, "a network failure is not an empty project");
const listing = normalizeListing({ path: root, entries: [{ name: "a.md", path: `${root}/a.md`, kind: "text" }, { name: "logo.png", path: `${root}/logo.png`, kind: "blob" }] });
equal(listing.entries?.map((entry) => entry.kind), ["page", "file"], "real daemon kinds map to rendering models");
equal(normalizePage({ path: `${root}/a.md`, kind: "text", text: "# Real content", rev: "r1" }).text, "# Real content", "normalization retains text and revision");
equal(normalizeSearch({ matches: [{ path: `${root}/a.md`, line: 2, text: "Real match" }] }).hits?.[0].snippet, "Real match", "daemon search results render");
const nativeRenamedPage = normalizeSearch({ matches: [{ path: `${root}/notes.pdf`, text: "Real match", source: `${root}/notes.pdf` }] });
equal(nativeRenamedPage.hits?.[0].kind, "page", "native indexed text stays a page even with an attachment-like name");
const legacyHostedBlob = normalizeSearch({ matches: [{ path: `${root}/report.pdf`, text: "Real match", source: "extracted" }] });
equal(legacyHostedBlob.hits?.[0].kind, "file", "older hosted extracted matches remain files");
// Hosted Worker (agenticos-v2 #1652): `kind` says page or attachment, `source`
// says what matched, and `partial` discloses the any-term fallback. Native
// search returns text pages and a source path but no kind or partial flag.
const hostedSearch = normalizeSearch({
  matches: [
    { path: `${root}/a.md`, line: 1, text: "needle", kind: "text", source: "page", rev: "r1" },
    { path: `${root}/report.pdf`, line: 1, text: "report.pdf", kind: "blob", source: "name", rev: "d1" },
  ],
  partial: true,
});
equal(hostedSearch.hits?.map((hit) => hit.kind), ["page", "file"], "hosted kinds are carried, not assumed to be pages");
equal(hostedSearch.hits?.[1].name, "report.pdf", "an attachment filename result keeps its name");
equal(hostedSearch.partial, true, "the partial-search fallback is disclosed");
equal(normalizeSearch({ matches: [{ path: `${root}/a.md`, line: 2, text: "Real match" }] }).partial, undefined, "a native reply claims neither partial nor complete");
const history = normalizeHistory({ path: `${root}/a.md`, commits: [{ sha: "sha1", at: 1, subject: "Updated brief", actor: "master" }] });
equal(history.entries?.[0].author, "master", "daemon history attributes the author");
equal(history.canCompare, false, "unsupported history actions are not offered");
// Hosted history: the board's current revision and per-row availability travel
// through, so the pane never pins the current row to a historical selector.
const hostedHistory = normalizeHistory({
  path: `${root}/a.md`,
  rev: "rev-current",
  commits: [
    { sha: "rev-current", at: 2, subject: "wiki: write", actor: "usr_1", can_open: true },
    { sha: "rev-old", at: 1, subject: "wiki: write", actor: "usr_1", can_open: false },
  ],
});
equal(hostedHistory.rev, "rev-current", "the board's current revision is not guessed from the first row");
equal(hostedHistory.entries?.map((entry) => entry.can_open), [true, false], "each revision carries its own availability");
const from = { screen: "projects", section: "context", slug: "cadence" } as const;
equal(contextNavigationSearch(from, { ...from, slug: "other" }, "?file=a.md&mode=edit&q=old&issue=CAD-1"), "issue=CAD-1", "changing projects clears context navigation, keeps drawer");
equal(contextNavigationSearch(from, { ...from, section: "epics" }, "?file=a.md&mode=edit"), "", "context state does not leak into other tabs");
console.log("project wiki checks passed");
