import type { WikiEntry } from "./api";

/**
 * The folder tree's view-model (CAD-581): rows in display order with the
 * depth the indent needs, the caret state and the lock badge. Pure, so
 * the tree's rendering rules are unit-tested (tests/wikiTree.test.ts).
 */

export interface TreeRow {
  path: string;
  name: string;
  kind: WikiEntry["kind"];
  depth: number;
  /** A folder: the caret toggles it. */
  dir: boolean;
  expanded: boolean;
  /** Read-only for this caller — the row shows the lock badge. */
  locked: boolean;
  /** Locked because the rules say operator-only, not just the server flag. */
  operatorOnly: boolean;
  /** This row is the caller's own agent folder. */
  own: boolean;
  size?: number;
  childCount?: number;
}

/**
 * Folders the rules keep operator-only (CAD-580): `global/`, and every
 * agent's `profile/` and `memory/` — the read-only views over the agent's
 * own files. The server sends the truth per entry (`locked`); this is the
 * badge's fallback when it does not.
 */
export function operatorOnly(path: string): boolean {
  const parts = path.split("/").filter(Boolean);
  if (parts[0] === "global") return true;
  if (parts[0] === "agents" && (parts[2] === "profile" || parts[2] === "memory")) return true;
  return false;
}

/** Read-only for this caller: the server's flag wins, the rule is the fallback. */
export function isLocked(entry: Pick<WikiEntry, "path" | "locked" | "read_only">): boolean {
  return entry.locked ?? entry.read_only ?? operatorOnly(entry.path);
}

/** Folders first, then name, case-insensitive. */
export function entryOrder(a: WikiEntry, b: WikiEntry): number {
  const dirs = Number(b.kind === "dir") - Number(a.kind === "dir");
  return dirs !== 0 ? dirs : a.name.localeCompare(b.name, undefined, { sensitivity: "base" });
}

/**
 * Flatten the loaded listings into rows. `children` holds the entries of
 * every folder fetched so far (root under `""`); only expanded folders
 * contribute their children, so a collapsed folder costs one row.
 */
export function treeRows(
  children: Record<string, WikiEntry[]>,
  expanded: ReadonlySet<string>,
  opts: { root?: string; self?: string | null } = {},
): TreeRow[] {
  const root = opts.root ?? "";
  const rows: TreeRow[] = [];
  const walk = (dir: string, depth: number) => {
    for (const entry of [...(children[dir] ?? [])].sort(entryOrder)) {
      const dir_ = entry.kind === "dir";
      const open = dir_ && expanded.has(entry.path);
      rows.push({
        path: entry.path,
        name: entry.name,
        kind: entry.kind,
        depth,
        dir: dir_,
        expanded: open,
        locked: isLocked(entry),
        operatorOnly: operatorOnly(entry.path),
        own: opts.self != null && entry.path === `agents/${opts.self}`,
        size: entry.size,
        childCount: entry.entries,
      });
      if (open) walk(entry.path, depth + 1);
    }
  };
  walk(root, 0);
  return rows;
}

/** The folders on the path from the root to `path` (for auto-expanding). */
export function ancestors(path: string): string[] {
  const parts = path.split("/").filter(Boolean);
  const out: string[] = [];
  for (let i = 0; i < parts.length; i += 1) out.push(parts.slice(0, i + 1).join("/"));
  return out;
}
