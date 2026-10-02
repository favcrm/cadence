import { screenProjection, screenTag } from "../src/features/workspace-apps/screen/screenProjection";
import type { Installation, AppContext, WorkspaceRun, AppEffect } from "../src/features/workspace-apps/workspaceApps";
function check(value: unknown, message: string) { if (!value) throw new Error(message); }
const installation = { install_id: "our", digest: "sha256:a", name: "portable", title: "App", version: "1", summary: "summary", files: ["screens/main/screens.json"], approved: true, executable: true } as Installation;
check(screenTag(installation) === "main", "generic discovery");
check(screenTag({ ...installation, approved: false }) === null, "approval mandatory");
check(screenTag({ ...installation, files: [...installation.files, "screens/other/screens.json"] }) === null, "ambiguous screens refuse");
const context = { id: "brand", install_id: "our", revision: 1, digest: "sha256:context", state: "active", config: { schema: 1, label: "Our brand", input_defaults: { SECRET: "private default" } } } as AppContext;
const run = { id: "run", install_id: "our", context_id: "brand", state: "succeeded", snapshot_digest: "sha256:b", snapshot: { workflow: { title: "Actual workflow" }, inputs: { source: "PRIVATE SOURCE", cookie: "PRIVATE COOKIE" }, assignments: { writer: { alias: "secret agent" } } } } as unknown as WorkspaceRun;
const effect = { effect_id: "effect", state: "done", authority: { install_id: "our", run_id: "run", context: { id: "brand" }, binding: { token: "PRIVATE TOKEN" } }, record: { title: "Actual effect", input: { text: "PRIVATE POST" }, preview: "PRIVATE URL" } } as unknown as AppEffect;
const projected = screenProjection(installation, "main", "brand", [context, { ...context, id: "foreign", install_id: "other" }], [run, { ...run, id: "foreign run", context_id: "foreign" }], [effect, { ...effect, effect_id: "foreign effect", authority: { ...effect.authority, install_id: "other" } }]);
check(projected.contexts.length === 1 && projected.runs.length === 1 && projected.outbox.length === 1, "exact scope only");
check(projected.runs[0].state === "succeeded" && projected.runs[0].title === "Actual workflow", "run status and title remain genuine");
check(!JSON.stringify(projected).includes("PRIVATE") && !JSON.stringify(projected).includes("secret agent"), "raw secrets, inputs, config and provider fields excluded");
let refused = false;
try { screenProjection(installation, "main", "unavailable", [context], [run], [effect]); } catch { refused = true; }
check(refused, "unavailable context fails closed");
console.log("screen projection scope and privacy checks pass");
