import type { Installation } from "./workspaceApps";

/** Exact Social Content versions the native workspace knows, each with its exact workflow set. */
const fiveWorkflows = ["instagram", "facebook", "source-instagram", "image-instagram", "image-manual"] as const;
/** CAD-1143 P4: Redo adds exactly `redo-text` + `redo-image`; the five stay unchanged. */
const sevenWorkflows = [...fiveWorkflows, "redo-text", "redo-image"] as const;
const socialContentWorkflows = new Map<string, readonly string[]>([
  ["0.2.0", ["instagram", "facebook"]],
  ["0.3.0", ["instagram", "facebook", "source-instagram"]],
  ["0.4.0", ["instagram", "facebook", "source-instagram", "image-instagram"]],
  ["0.5.0", fiveWorkflows],
  // CAD-1038: the independent package (cadence-app-social-content) ships the 0.5.0
  // workflows byte-identical; 0.5.2 is live and 0.5.3 is next.
  ["0.5.2", fiveWorkflows],
  ["0.5.3", fiveWorkflows],
  // CAD-1123 (operator decision Q4): the interim simple-UX package keeps the
  // same five workflows and the `?screen=native` back door until H4.
  ["0.5.4", fiveWorkflows],
  // CAD-1143: the integrated publish frame and Settings (package PR7).
  // Byte-identical workflow set to 0.5.2 per the PR7 report; the digest pin
  // for the re-cut source-instagram.md (profile_handle context_default)
  // lands with the package pins, guarded by the exact-set+exact-bytes check.
  ["0.6.0", fiveWorkflows],
  // CAD-1143 P4: genuine Redo (`redo-text`, `redo-image`). Existing five
  // unchanged; UI controls stay off pending acceptance. No wildcard, no
  // invented hashes — the exact-set guard below admits only these seven.
  ["0.7.0", sevenWorkflows],
]);

export function supportsSocialContentWorkspace(installation: Pick<Installation, "name" | "version" | "files">): boolean {
  const expected = socialContentWorkflows.get(installation.version);
  if (installation.name !== "social-content" || !expected) return false;
  const installed = installation.files
    .filter(path => path.startsWith("workflows/") && path.endsWith(".md"))
    .map(path => path.slice("workflows/".length, -".md".length));
  return installed.length === expected.length && expected.every(name => installed.includes(name));
}
