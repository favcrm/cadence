import { makePlanner, runCall, type ActionContext } from "../src/features/workspace-apps/screen/screenActions";
import { accountLabel, connectOutcome, connectReturnTo, connectToast, publicationBinding, sendSlot, withoutConnectParams } from "../src/features/workspace-apps/socialConnect";
import type { AppBinding, Installation } from "../src/features/workspace-apps/workspaceApps";

// CAD-1290: the hosted connect link, the return signal and "Use for publishing".
declare function require(name: string): any;
function equal(actual: unknown, expected: unknown, why: string) {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) throw new Error(`${why}: ${JSON.stringify(actual)} !== ${JSON.stringify(expected)}`);
}
function assert(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }

const nodeCrypto = require("crypto");
Object.defineProperty(globalThis, "crypto", { configurable: true, value: { getRandomValues: nodeCrypto.webcrypto.getRandomValues.bind(nodeCrypto.webcrypto) } });
(globalThis as any).window = { location: { origin: "https://ef.cadencecloud.app" } };
type Call = { path: string; body: unknown };
const calls: Call[] = [];
let replies: Record<string, { status?: number; body: unknown }> = {};
globalThis.fetch = async (input, init) => {
  const path = String(input);
  calls.push({ path, body: init?.body ? JSON.parse(String(init.body)) : undefined });
  const key = Object.keys(replies).find(prefix => path.endsWith(prefix));
  const hit = key ? replies[key] : { status: 404, body: { error: "no route" } };
  return new Response(JSON.stringify(hit.body), { status: hit.status ?? 200, headers: { "Content-Type": "application/json" } });
};

const installation = { install_id: "inst1", digest: "sha256:d1", capabilities: {
  publication: { schema: 1, capability: "text.publish", version: 1, action: "publish", resource_kind: "connection_account", effect: "send" },
  source: { schema: 1, capability: "social.read", version: 1, action: "list_posts", resource_kind: "connection_account", effect: "read" } } } as unknown as Installation;
const ctx = (changed: () => void = () => {}): ActionContext => ({ installation, installId: "inst1", contextId: "ctx1", runs: [], onChanged: changed });
const binding = (publish?: { destination_id: string; destination_label: string; toolkit: string }, over: Partial<AppBinding> = {}): AppBinding => ({
  id: "b1", install_id: "inst1", context_id: "ctx1", slot: "publication", revision: 3, state: "configured", digest: "x",
  config: { bundle_digest: "sha256:d1", connection_id: "c", provider: "local", account: "local", mapping: {} as never, ...(publish ? { publish } : {}) }, ...over });

async function main() {
  // The return signal: only Instagram's three outcomes, and the params leave the URL.
  equal(connectOutcome("?aos_connect=connected&toolkit=instagram"), "connected", "connected");
  equal(connectOutcome("?aos_connect=failed&toolkit=instagram&x=1"), "failed", "failed");
  equal(connectOutcome("?aos_connect=cancelled&toolkit=instagram"), "cancelled", "cancelled");
  equal(connectOutcome("?aos_connect=connected&toolkit=facebook"), null, "another toolkit is not ours");
  equal(connectOutcome("?aos_connect=connected"), null, "a missing toolkit is not ours");
  equal(connectOutcome("?aos_connect=hacked&toolkit=instagram"), null, "an unknown outcome is not ours");
  equal(withoutConnectParams("?aos_connect=connected&toolkit=instagram&screen=native"), "?screen=native", "other params stay");
  equal(withoutConnectParams("?aos_connect=failed&toolkit=instagram"), "", "nothing left");
  equal(connectToast("connected").text, "Instagram connected", "connected copy");
  equal(connectToast("failed").text, "Connection failed — try again", "failed copy");
  equal(connectToast("cancelled"), connectToast("failed"), "cancelled reads as not connected");
  equal(connectReturnTo("https://ef.cadencecloud.app", "inst 1"), "https://ef.cadencecloud.app/app-installations/inst%201?screen=native", "return_to is this board's host Settings page");
  equal([accountLabel("harbour"), accountLabel("@harbour"), accountLabel("Harbour Cafe")], ["@harbour", "@harbour", "Harbour Cafe"], "handles get an @");
  equal(sendSlot(installation), "publication", "one send slot");
  equal(sendSlot({ ...installation, capabilities: {} } as Installation), null, "no send slot");
  assert(publicationBinding([binding()], "publication", "ctx1", "sha256:d1"), "live binding found");
  assert(!publicationBinding([binding()], "publication", null, "sha256:d1"), "another scope is not the binding");
  assert(!publicationBinding([binding()], "publication", "ctx1", "sha256:old"), "an old bundle is not the binding");
  assert(!publicationBinding([binding(undefined, { state: "revoked" })], "publication", "ctx1", "sha256:d1"), "a revoked binding is not the binding");

  // Hosted: the app's Manage connections link opens the host-composed AgenticOS URL top-level.
  replies = { "/publishing/connect-link": { body: { hosted: true, url: "https://api.example/account/ws/connections/connect?toolkit=instagram&return_to=x" } } };
  let assigned: string | null = null;
  let shown: string | null = null;
  let navigated: string | null = null;
  const ui = { showLink: (url: string) => { shown = url; }, navigate: (path: string) => { navigated = path; }, assign: (url: string) => { assigned = url; } };
  let result = await runCall(ctx(), "open-link", { url: "/settings/connections" }, ui);
  assert(result.ok && assigned === "https://api.example/account/ws/connections/connect?toolkit=instagram&return_to=x" && shown === null, "hosted opens the AgenticOS link top-level");
  equal(calls[0], { path: "/api/app-installations/inst1/publishing/connect-link", body: { return_to: "https://ef.cadencecloud.app/app-installations/inst1?screen=native" } }, "the host asks with its own return_to");
  // The frame never supplies the URL: another url is not routed through the connect link.
  assigned = null; calls.length = 0;
  result = await runCall(ctx(), "open-link", { url: "https://evil.example/x" }, ui);
  assert(!result.ok && assigned === null && calls.length === 0, "an arbitrary link neither connects nor reaches the daemon");
  // Hosted without a public AgenticOS address: refused, not sent to the local page.
  replies = { "/publishing/connect-link": { body: { hosted: true, unavailable: true } } };
  assigned = null; shown = null;
  result = await runCall(ctx(), "open-link", { url: "/settings/connections" }, ui);
  assert(!result.ok && assigned === null && navigated === null, "hosted without an origin is refused");
  // Local board: the daemon says not hosted, the local page link is kept.
  replies = { "/publishing/connect-link": { body: { hosted: false } } };
  assigned = null;
  result = await runCall(ctx(), "open-link", { url: "/settings/connections" }, ui);
  assert(assigned === null && navigated === "/settings/connections", "a local board keeps /settings/connections");

  // Destinations list verb: exact args, label from the host.
  replies = { "/publishing/destinations": { body: { unavailable: false, destinations: [{ destination_id: "d1", label: "harbour", toolkit: "instagram" }] } } };
  result = await runCall(ctx(), "social.destinations.list", {}, ui);
  equal(result, { ok: true, data: { unavailable: false, destinations: [{ destination_id: "d1", label: "@harbour" }] } }, "list");
  result = await runCall(ctx(), "social.destinations.list", { x: 1 }, ui);
  assert(!result.ok, "list takes no arguments");

  // Use for publishing: a plan with a host label; the run sends the revision it read.
  replies = { "/publishing/destinations": { body: { unavailable: false, destinations: [{ destination_id: "d1", label: "harbour", toolkit: "instagram" }] } },
    "/contexts/ctx1/bindings": { body: { bindings: [binding({ destination_id: "d0", destination_label: "old", toolkit: "instagram" })] } },
    "/publishing/use": { body: { binding: binding() } } };
  calls.length = 0;
  let changed = 0;
  const planned = await makePlanner(() => ctx(() => { changed += 1; }))("publish.destination.use", { destination_id: "d1" });
  assert("run" in planned && planned.label === "Use @harbour for publishing", "label comes from the company's list");
  const done = await planned.run();
  assert(done.ok && changed === 1, "the tap runs and the board reloads");
  equal(calls[calls.length - 1], { path: "/api/app-installations/inst1/publishing/use", body: { destination_id: "d1", context_id: "ctx1", expected_revision: 3 } }, "replace sends the revision it saw");
  // An account that is not in the company's list never gets a button.
  const missing = await makePlanner(() => ctx())("publish.destination.use", { destination_id: "other-company" });
  assert("code" in missing && missing.code === "unknown_destination", "a destination outside the list is refused before any tap");
  // A forged field (a grant, a label) or no destination is refused.
  for (const args of [{}, { destination_id: "d1", grant_id: "dpq_abcdefgh" }, { destination_id: "d1", label: "forged" }]) {
    const refused = await makePlanner(() => ctx())("publish.destination.use", args as Record<string, unknown>);
    assert("code" in refused && refused.code === "bad_args", "bad args refused");
  }
  // No existing binding: create with a request id, no revision.
  replies["/contexts/ctx1/bindings"] = { body: { bindings: [] } };
  calls.length = 0;
  const fresh = await makePlanner(() => ctx())("publish.destination.use", { destination_id: "d1" });
  assert("run" in fresh, "plans");
  await fresh.run();
  const sent = calls[calls.length - 1].body as Record<string, unknown>;
  assert(typeof sent.request_id === "string" && !("expected_revision" in sent), "create carries a request id and no revision");
}
main().then(() => console.log("socialConnect ok"));
