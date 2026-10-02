import type { Installation } from "./workspaceApps";

/** Exact Social Content versions the native workspace knows, each with its exact workflow set. */
const fiveWorkflows = ["instagram", "facebook", "source-instagram", "image-instagram", "image-manual"] as const;
const socialContentWorkflows = new Map<string, readonly string[]>([
  ["0.2.0", ["instagram", "facebook"]],
  ["0.3.0", ["instagram", "facebook", "source-instagram"]],
  ["0.4.0", ["instagram", "facebook", "source-instagram", "image-instagram"]],
  ["0.5.0", fiveWorkflows],
  // CAD-1038: the independent package (cadence-app-social-content) ships the 0.5.0
  // workflows byte-identical; 0.5.2 is live and 0.5.3 is next.
  ["0.5.2", fiveWorkflows],
  ["0.5.3", fiveWorkflows],
]);

export function supportsSocialContentWorkspace(installation: Pick<Installation, "name" | "version" | "files">): boolean {
  const expected = socialContentWorkflows.get(installation.version);
  if (installation.name !== "social-content" || !expected) return false;
  const installed = installation.files
    .filter(path => path.startsWith("workflows/") && path.endsWith(".md"))
    .map(path => path.slice("workflows/".length, -".md".length));
  return installed.length === expected.length && expected.every(name => installed.includes(name));
}
