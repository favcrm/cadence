/**
 * Wiki paths are root-relative and `/`-joined (`projects/cadence/notes.md`);
 * the root itself is the empty string. These helpers are the only place
 * that knows the shape, so the tree, breadcrumbs and the URL routes agree
 * (CAD-581).
 */

/** Split a path into its segments, dropping empties and stray slashes. */
export function splitPath(path: string): string[] {
  return path.split("/").filter(Boolean);
}

/** The last segment — the file or folder name. */
export function baseName(path: string): string {
  const parts = splitPath(path);
  return parts.length ? parts[parts.length - 1] : "wiki";
}

/** The containing folder; the root for a top-level entry. */
export function parentPath(path: string): string {
  const parts = splitPath(path);
  parts.pop();
  return parts.join("/");
}

export function joinPath(...parts: string[]): string {
  return parts.flatMap(splitPath).join("/");
}

export interface Crumb {
  name: string;
  path: string;
}

/** The breadcrumb trail, root excluded — `global/` for `global/glossary.md`. */
export function breadcrumbs(path: string): Crumb[] {
  const out: Crumb[] = [];
  let acc = "";
  for (const part of splitPath(path)) {
    acc = acc ? `${acc}/${part}` : part;
    out.push({ name: part, path: acc });
  }
  return out;
}

/** A path as URL segments (`projects/cadence/notes.md` → the same, escaped). */
export function encodePath(path: string): string {
  return splitPath(path).map(encodeURIComponent).join("/");
}

/** The folder a new page or upload lands in, given the open path and kind. */
export function targetDir(path: string, kind: "dir" | "page" | "file"): string {
  return kind === "dir" ? path : parentPath(path);
}
