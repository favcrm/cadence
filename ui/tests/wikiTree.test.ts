import { ancestors, entryOrder, isLocked, operatorOnly, treeRows } from "../src/features/wiki/tree";
import { baseName, breadcrumbs, encodePath, joinPath, parentPath, targetDir } from "../src/features/wiki/paths";
import type { WikiEntry } from "../src/features/wiki/api";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

const dir = (path: string, extra: Partial<WikiEntry> = {}): WikiEntry => ({
  name: baseName(path),
  path,
  kind: "dir",
  ...extra,
});
const page = (path: string, extra: Partial<WikiEntry> = {}): WikiEntry => ({
  name: baseName(path),
  path,
  kind: "page",
  size: 2100,
  ...extra,
});
const blob = (path: string, extra: Partial<WikiEntry> = {}): WikiEntry => ({
  name: baseName(path),
  path,
  kind: "file",
  mime: "image/png",
  size: 412_000,
  ...extra,
});

// ---- paths ---------------------------------------------------------------

equal(parentPath("projects/cadence/notes.md"), "projects/cadence", "parent of a page");
equal(parentPath("global"), "", "parent of a root entry");
equal(baseName("projects/cadence/notes.md"), "notes.md", "basename");
equal(baseName(""), "wiki", "the root's name");
equal(joinPath("projects", "cadence", "notes.md"), "projects/cadence/notes.md", "join");
equal(joinPath("projects/", "/cadence"), "projects/cadence", "join normalises slashes");
equal(
  breadcrumbs("projects/cadence/notes.md").map((c) => `${c.name}@${c.path}`),
  ["projects@projects", "cadence@projects/cadence", "notes.md@projects/cadence/notes.md"],
  "breadcrumbs accumulate",
);
equal(encodePath("projects/my notes/é.md"), "projects/my%20notes/%C3%A9.md", "url segments escape");
equal(targetDir("projects/cadence/design", "dir"), "projects/cadence/design", "a folder uploads into itself");
equal(targetDir("projects/cadence/notes.md", "page"), "projects/cadence", "a page uploads beside itself");

// ---- lock rules (CAD-580's allowlist, mirrored for the badge) -------------

equal(operatorOnly("global"), true, "global/ is operator-only");
equal(operatorOnly("global/glossary.md"), true, "so is anything under it");
equal(operatorOnly("agents/master/profile"), true, "an agent's profile is operator-only");
equal(operatorOnly("agents/master/memory"), true, "the memory view is read-only");
equal(operatorOnly("agents/swe-1/knowledge"), false, "own knowledge is writable");
equal(operatorOnly("projects/cadence/notes.md"), false, "a project page is writable");
equal(operatorOnly("users/operator/notes.md"), false, "users/ is a rule the server enforces, not a badge rule");

equal(isLocked({ path: "global", locked: false }), false, "the server's flag wins when it says writable");
equal(isLocked({ path: "projects/cadence/notes.md", locked: true }), true, "the server's flag wins when locked");
equal(isLocked({ path: "global/x.md" }), true, "the rule is the fallback");
equal(isLocked({ path: "agents/master/profile/SOUL.md", read_only: true }), true, "read_only reads as locked");
equal(isLocked({ path: "agents/swe-1/knowledge/x.md" }), false, "own folder is not locked");

// ---- tree rows -----------------------------------------------------------

const children: Record<string, WikiEntry[]> = {
  "": [dir("projects"), dir("agents"), dir("global")],
  projects: [dir("projects/cadence"), page("projects/readme.md")],
  "projects/cadence": [dir("projects/cadence/design"), page("projects/cadence/notes.md")],
  "projects/cadence/design": [page("projects/cadence/design/tokens.md"), blob("projects/cadence/design/board-v5.png")],
  agents: [dir("agents/master"), dir("agents/swe-1")],
  "agents/master": [dir("agents/master/profile"), dir("agents/master/knowledge"), dir("agents/master/memory")],
};

const collapsed = treeRows(children, new Set([""]));
equal(
  collapsed.map((r) => `${r.depth}:${r.name}`),
  ["0:agents", "0:global", "0:projects"],
  "folders first, case-insensitive, and a collapsed folder costs one row",
);
equal(collapsed.every((r) => r.dir && !r.expanded), true, "collapsed rows carry no caret state");

const opened = treeRows(children, new Set(["", "projects", "projects/cadence", "agents", "agents/master"]));
equal(
  opened.map((r) => `${"  ".repeat(r.depth)}${r.name}`),
  [
    "agents",
    "  master",
    "    knowledge",
    "    memory",
    "    profile",
    "  swe-1",
    "global",
    "projects",
    "  cadence",
    "    design",
    "    notes.md",
    "  readme.md",
  ],
  "expanded folders contribute their children at the right depth",
);

const badge = treeRows(children, new Set(["", "agents", "agents/master"]));
const locked = badge.filter((r) => r.locked).map((r) => r.path);
equal(
  locked,
  ["agents/master/memory", "agents/master/profile", "global"],
  "the lock badge marks the read-only rows",
);
equal(badge.find((r) => r.path === "agents/master/knowledge")?.locked, false, "knowledge/ has no badge");

const own = treeRows(children, new Set(["", "agents"]), { self: "swe-1" });
equal(own.find((r) => r.path === "agents/swe-1")?.own, true, "the caller's own folder is marked");
equal(own.find((r) => r.path === "agents/master")?.own, false, "another agent's is not");

equal(
  [...(children["projects/cadence/design"] ?? [])].sort(entryOrder).map((e) => e.name),
  ["board-v5.png", "tokens.md"],
  "files keep name order",
);
equal(ancestors("projects/cadence/design"), ["projects", "projects/cadence", "projects/cadence/design"], "ancestors");

console.log("wiki tree checks passed");
