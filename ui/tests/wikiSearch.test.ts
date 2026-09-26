import { filterHits, hitKind, snippetParts, TYPE_FILTERS, type SearchHit } from "../src/features/wiki/search";
import { blobPage } from "../src/features/wiki/api";
import { extOf, kindLabel, previewKind } from "../src/features/wiki/preview";
import { relTime, versionRows, versionWho } from "../src/features/wiki/history";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

// ---- search results ------------------------------------------------------

const hits: SearchHit[] = [
  { path: "projects/cadence/design/tokens.md", name: "tokens.md", kind: "page", snippet: "every mockup starts from the real tokens" },
  { path: "projects/cadence/design/board-v5.png", name: "board-v5.png", mime: "image/png", snippet: "design tokens board" },
  { path: "projects/cadence/design/demo-cut.mp4", name: "demo-cut.mp4", mime: "video/mp4", snippet: "tokens in motion" },
  { path: "global/spec-draft.pdf", name: "spec-draft.pdf", mime: "application/pdf", snippet: "§3.1 tokens and theming" },
  { path: "global/exports.zip", name: "exports.zip", mime: "application/zip", snippet: "tokens export" },
];

equal(TYPE_FILTERS, ["all", "pages", "images", "video", "pdf"], "the chips");
equal(hits.map(hitKind), ["pages", "images", "video", "pdf", "pages"], "each hit's kind");
equal(filterHits(hits, "all").length, 5, "all keeps everything");
equal(filterHits(hits, "pages").map((h) => h.name), ["tokens.md", "exports.zip"], "pages keeps md and other text");
equal(filterHits(hits, "images").map((h) => h.name), ["board-v5.png"], "images");
equal(filterHits(hits, "video").map((h) => h.name), ["demo-cut.mp4"], "video");
equal(filterHits(hits, "pdf").map((h) => h.name), ["spec-draft.pdf"], "pdf");
equal(filterHits([], "images"), [], "an empty result stays empty");

// The snippet's query occurrences are the only marked parts.
equal(snippetParts("every mockup starts from the real tokens", "tokens"), [
  { text: "every mockup starts from the real ", mark: false },
  { text: "tokens", mark: true },
], "the match is marked");
equal(snippetParts("Tokens, then tokens again", "tokens"), [
  { text: "Tokens", mark: true },
  { text: ", then ", mark: false },
  { text: "tokens", mark: true },
  { text: " again", mark: false },
], "every occurrence, case-insensitively");
equal(snippetParts("no match here", "tokens"), [{ text: "no match here", mark: false }], "no match, no marks");
equal(snippetParts("anything", ""), [{ text: "anything", mark: false }], "an empty query marks nothing");
equal(snippetParts("", "tokens"), [], "an empty snippet has no parts");

// ---- preview decisions (security: never render uploads as markup) --------

equal(previewKind("board-v5.png", "image/png"), "image", "a png previews");
equal(previewKind("photo.JPG", null), "image", "so does a jpg, case-insensitively");
equal(previewKind("demo-cut.mp4", null), "video", "a video plays");
equal(previewKind("spec-draft.pdf", "application/pdf"), "pdf", "a pdf previews in the sandboxed frame");
equal(previewKind("notes.md", "text/markdown"), "download", "markdown is a page, never a blob preview");
equal(previewKind("exports.zip", "application/zip"), "download", "a zip downloads");
equal(previewKind("evil.svg", "image/svg+xml"), "download", "an uploaded svg downloads");
equal(previewKind("evil.svg", null), "download", "even with no mime");
equal(previewKind("evil.html", "text/html"), "download", "an uploaded html downloads");
equal(previewKind("evil.htm", null), "download", "and so does .htm");
equal(previewKind("shot.png", "text/html"), "download", "a lying mime never renders markup");
equal(previewKind("notes.xml", "application/xml"), "download", "xml downloads");
equal(extOf("archive.tar.gz"), "gz", "the last extension wins");
equal(extOf("noext"), "", "no extension");
equal(kindLabel("board-v5.png", "image/png"), "IMG", "the tile chip for an image");
equal(kindLabel("exports.zip", null), "ZIP", "the tile chip for a zip");
equal(kindLabel("noext", null), "FILE", "the fallback chip");

// A blob's preview reads its listing entry — the file route streams the
// bytes, so the preview never asks it for JSON (which would be null).
equal(
  blobPage({
    path: "projects/cadence/design/board-v5.png",
    name: "board-v5.png",
    kind: "file",
    size: 412 * 1024,
    mime: "image/png",
    rev: "b5a1c02",
    edited_by: "swe-1",
    mtime: "2026-09-26T09:00:00Z",
  }),
  {
    path: "projects/cadence/design/board-v5.png",
    kind: "file",
    mime: "image/png",
    size: 412 * 1024,
    rev: "b5a1c02",
    edited_by: "swe-1",
    mtime: "2026-09-26T09:00:00Z",
  },
  "a blob previews from its listing entry",
);

// ---- history -------------------------------------------------------------

const now = Date.parse("2026-09-26T12:00:00Z");
equal(relTime("2026-09-26T11:59:40Z", now), "just now", "seconds");
equal(relTime("2026-09-26T11:30:00Z", now), "30 min ago", "minutes");
equal(relTime("2026-09-26T10:00:00Z", now), "2 h ago", "hours");
equal(relTime("2026-09-25T09:00:00Z", now), "yesterday", "yesterday");
equal(relTime("2026-09-23T12:00:00Z", now), "3 d ago", "days");
equal(relTime("2026-09-19T12:00:00Z", now), "1 wk ago", "weeks");
equal(relTime("not a date", now), "not a date", "non-ISO input passes through");
equal(relTime(null, now), "", "no timestamp, no label");

const rows = versionRows(
  [
    { rev: "a41f9c2", author: "master", at: "2026-09-26T10:00:00Z", summary: '"tokens not literals" note' },
    { rev: "f0c33d8", author: "swe-1", at: "2026-09-25T09:00:00Z", summary: "open questions section" },
    { rev: "7be1a04", author: "swe-1", at: "2026-09-23T12:00:00Z", summary: "first draft" },
  ],
  "a41f9c2",
  now,
);
equal(rows.map((r) => r.current), [true, false, false], "the current revision is marked");
equal(versionWho(rows[0]), "master · current", "the current row's label");
equal(versionWho(rows[1]), "swe-1", "an older row keeps its author");
equal(rows.map((r) => r.when), ["2 h ago", "yesterday", "3 d ago"], "the rows' relative times");
equal(versionRows([], null, now), [], "no history, no rows");

console.log("wiki search checks passed");
