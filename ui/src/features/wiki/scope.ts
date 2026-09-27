export interface WikiScope {
  root: string;
  label: string;
}

/** This is a navigation boundary; the daemon remains the authority for access. */
export function withinScope(path: string, root: string): boolean {
  if (path.split("/").some((part) => part === "." || part === "..") || /[\\%\x00-\x1f]/.test(path)) return false;
  return root === "" || path === root || path.startsWith(`${root}/`);
}

export function scopedAncestors(path: string, root: string): string[] {
  const dirs = [root];
  const relative = path === root ? "" : path.slice(root ? root.length + 1 : 0);
  let current = root;
  for (const part of relative.split("/").filter(Boolean)) {
    current = current ? `${current}/${part}` : part;
    dirs.push(current);
  }
  return dirs;
}
