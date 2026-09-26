import {
  conflictFrom,
  conflictText,
  draftKey,
  dropDraft,
  readDraft,
  saveBody,
  stashDraft,
} from "../src/features/wiki/editor";
import { WikiError, wikiErrorFrom } from "../src/features/wiki/api";
import { diffCounts, diffLabel, lineDiff, unifiedDiffLines } from "../src/features/wiki/diff";
import type { StorageLike } from "../src/lib/draft";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

/** The smallest sessionStorage stand-in the draft needs. */
function fakeStorage(seed: Record<string, string> = {}): StorageLike & { map: Map<string, string> } {
  const map = new Map(Object.entries(seed));
  return {
    map,
    getItem: (k) => map.get(k) ?? null,
    setItem: (k, v) => void map.set(k, v),
    removeItem: (k) => void map.delete(k),
  };
}

// ---- the draft in sessionStorage ----------------------------------------

const storage = fakeStorage();
const draft = { path: "projects/cadence/notes.md", text: "half an edit", baseRev: "a41f9c2", at: 1 };
stashDraft(storage, draft);
equal(storage.map.has(draftKey(draft.path)), true, "the draft is stored under its path");
equal(readDraft(storage, draft.path), draft, "and reads back whole");
equal(readDraft(storage, "other.md"), null, "another page has no draft");
equal(draftKey("a/b.md"), "cadence.wiki.draft:a/b.md", "the key namespaces the path");

// An emptied editor drops the draft instead of storing an empty one.
stashDraft(storage, { ...draft, text: "" });
equal(readDraft(storage, draft.path), null, "an empty draft is dropped");

// A blocked store never throws — the edit stays in the textarea.
const blocked: StorageLike = {
  getItem: () => {
    throw new Error("blocked");
  },
  setItem: () => {
    throw new Error("blocked");
  },
  removeItem: () => {
    throw new Error("blocked");
  },
};
stashDraft(blocked, draft);
equal(readDraft(blocked, draft.path), null, "a blocked store reads as no draft");
dropDraft(blocked, draft.path);

// Corrupt JSON is ignored, not thrown.
const corrupt = fakeStorage({ [draftKey(draft.path)]: "{not json" });
equal(readDraft(corrupt, draft.path), null, "corrupt drafts are ignored");

// ---- the conflict path ---------------------------------------------------

const save = saveBody("projects/cadence/notes.md", "new text", "a41f9c2");
equal(save, { path: "projects/cadence/notes.md", text: "new text", if_rev: "a41f9c2" }, "the save carries if_rev");

// The server refuses a stale rev with the rev that won.
const refused = wikiErrorFrom(409, { error: "conflict", code: "conflict", rev: "f0c33d8", author: "master" });
equal(refused.status, 409, "a refusal keeps its status");
const conflict = conflictFrom(refused);
equal(conflict, { rev: "f0c33d8", author: "master", at: null }, "the conflict carries the winning rev");
equal(conflictText(conflict!), "changed since you opened it — master saved rev f0c33d8", "the banner line");

// A conflict body with no author still reads.
equal(conflictText(conflictFrom(wikiErrorFrom(409, { rev: "9" }))!), "changed since you opened it — saved rev 9", "no author");

// A different failure is not a conflict — the toast path, not the banner.
equal(conflictFrom(wikiErrorFrom(500, { error: "boom" })), null, "a 500 is not a conflict");
equal(conflictFrom(wikiErrorFrom(403, { error: "read-only" })), null, "a refusal is not a conflict");
equal(conflictFrom(new Error("offline")), null, "a plain error is not a conflict");
equal(conflictFrom(null), null, "null is not a conflict");
equal(new WikiError("x", 409).message, "x", "WikiError keeps its message");

// A save that succeeded drops the draft; a refused one keeps it.
const kept = fakeStorage();
stashDraft(kept, draft);
conflictFrom(wikiErrorFrom(409, { rev: "f0c33d8" }));
equal(readDraft(kept, draft.path)?.text, "half an edit", "a refused save keeps the draft");
dropDraft(kept, draft.path);
equal(readDraft(kept, draft.path), null, "a successful save (or reload) drops it");

// ---- the diff the banner's "review diff" shows ---------------------------

const lines = lineDiff("a\nb\nc\n", "a\nB\nc\n");
equal(lines, [
  { kind: "ctx", text: "a" },
  { kind: "del", text: "b" },
  { kind: "add", text: "B" },
  { kind: "ctx", text: "c" },
  { kind: "ctx", text: "" },
], "the local diff marks the changed line");
equal(diffCounts(lines), { added: 1, removed: 1 }, "the change counts");
equal(lineDiff("same", "same"), [{ kind: "ctx", text: "same" }], "an unchanged text is all context");

// And the history's unified diff.
const unified = [
  "diff --git a/notes.md b/notes.md",
  "index 111..222 100644",
  "--- a/notes.md",
  "+++ b/notes.md",
  "@@ -8,3 +8,4 @@",
  " colours come from tokens",
  "-plus .main. No exceptions yet.",
  "+plus .main`.",
  "+- Right rail is optional.",
].join("\n");
equal(
  unifiedDiffLines(unified).map((l) => `${l.kind}:${l.text}`),
  [
    "hunk:diff --git a/notes.md b/notes.md",
    "hunk:index 111..222 100644",
    "hunk:--- a/notes.md",
    "hunk:+++ b/notes.md",
    "hunk:@@ -8,3 +8,4 @@",
    "ctx:colours come from tokens",
    "del:plus .main. No exceptions yet.",
    "add:plus .main`.",
    "add:- Right rail is optional.",
  ],
  "the unified diff parses into display lines",
);
equal(diffLabel("f0c33d8", "a41f9c2"), "diff — f0c33d8 → a41f9c2", "the diff heading");

console.log("wiki editor checks passed");
