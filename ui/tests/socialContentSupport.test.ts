export {};
/** CAD-1038: the native workspace accepts the independent 0.5.2/0.5.3 packages and nothing wider.
 *  CAD-1123 (operator decision Q4): plus the interim 0.5.4, same five workflows.
 *  CAD-1173: plus the 0.6.0 single-role package, same five workflow names. */
declare function require(name: string): any;
const { supportsSocialContentWorkspace } = require("../src/features/workspace-apps/socialContentSupport") as typeof import("../src/features/workspace-apps/socialContentSupport");
type Installation = Parameters<typeof supportsSocialContentWorkspace>[0];

function assert(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }
const five = ["instagram", "facebook", "source-instagram", "image-instagram", "image-manual"];
const install = (version: string, workflows: string[], name = "social-content") => ({
  name, version, files: ["app.md", "rubrics/brand.md", "screens/main/screens.json", ...workflows.map(flow => `workflows/${flow}.md`)],
}) as unknown as Installation;

for (const version of ["0.5.0", "0.5.2", "0.5.3", "0.5.4", "0.6.0"]) {
  assert(supportsSocialContentWorkspace(install(version, five)), `${version} with the five workflows is supported`);
  assert(!supportsSocialContentWorkspace(install(version, five.slice(0, 4))), `${version} missing image-manual is unsupported`);
  assert(!supportsSocialContentWorkspace(install(version, [...five, "linkedin"])), `${version} with an extra workflow is unsupported`);
  assert(!supportsSocialContentWorkspace(install(version, [...five.slice(0, 4), "linkedin"])), `${version} with a swapped workflow is unsupported`);
  assert(!supportsSocialContentWorkspace(install(version, five, "social-content-fork")), `${version} under another app name is unsupported`);
}
for (const version of ["0.5.1", "0.5.5", "0.6.1", "0.5.4-rc.1", "0.5.40", " 0.5.4", "0.5.2-rc.1", "0.5.20", " 0.5.2"])
  assert(!supportsSocialContentWorkspace(install(version, five)), `unknown version ${JSON.stringify(version)} is unsupported`);
assert(supportsSocialContentWorkspace(install("0.4.0", five.slice(0, 4))), "0.4.0 keeps its four-workflow set");
assert(!supportsSocialContentWorkspace(install("0.4.0", five)), "0.4.0 with image-manual is unsupported");
console.log("socialContentSupport: ok");
