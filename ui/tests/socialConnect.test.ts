import { makePlanner, runCall, type ActionContext } from "../src/features/workspace-apps/screen/screenActions";
import { accountLabel, connectOutcome, connectReturnTo, connectToast, publishAllowed, publicationBinding, sendSlot, withoutConnectParams } from "../src/features/workspace-apps/socialConnect";
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

const installation = { install_id: "inst1", name: "social-content", digest: "sha256:d1", capabilities: {
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
  equal(connectToast("connected", publishAllowed("?aos_connect=connected&toolkit=instagram&aos_publish=allowed")).text, "Publishing allowed", "allowed copy");
  equal(connectToast("connected", publishAllowed("?aos_connect=connected&toolkit=instagram&aos_publish=skipped")).text, "Instagram connected", "skipped keeps the connected copy");
  equal(withoutConnectParams("?aos_connect=connected&aos_publish=allowed&toolkit=instagram&screen=native"), "?screen=native", "aos_publish leaves the URL");
  equal(connectToast("cancelled"), connectToast("failed"), "cancelled reads as not connected");
  // CAD-1438: return_to spells the canonical `/apps/<key>` route.
  equal(connectReturnTo("https://ef.cadencecloud.app", "social content", "install-1"), "https://ef.cadencecloud.app/apps/social%20content?screen=native&install=install-1", "return_to pins this installation on the board Settings page");
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
  equal(calls[0], { path: "/api/app-installations/inst1/publishing/connect-link", body: { return_to: "https://ef.cadencecloud.app/apps/social-content?screen=native&install=inst1" } }, "the host asks with its own canonical return_to");
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

  // Use for publishing: the user's own choice is a direct call; the host sends the revision it read.
  replies = { "/publishing/destinations": { body: { unavailable: false, destinations: [{ destination_id: "d1", label: "harbour", toolkit: "instagram" }] } },
    "/contexts/ctx1/bindings": { body: { bindings: [binding({ destination_id: "d0", destination_label: "old", toolkit: "instagram" })] } },
    "/publishing/use": { body: { binding: binding() } } };
  calls.length = 0;
  let changed = 0;
  const done = await runCall(ctx(() => { changed += 1; }), "publish.destination.use", { destination_id: "d1" }, ui);
  assert(done.ok && changed === 1, "the user's own choice writes directly and the board reloads");
  equal(calls[calls.length - 1], { path: "/api/app-installations/inst1/publishing/use", body: { destination_id: "d1", context_id: "ctx1", expected_revision: 3 } }, "replace sends the revision it saw");
  // An account that is not in the company's list is refused.
  const missing = await runCall(ctx(), "publish.destination.use", { destination_id: "other-company" }, ui);
  assert(!missing.ok && missing.refusal.code === "unknown_destination", "a destination outside the list is refused before any write");
  // A forged field (a grant, a label) or no destination is refused.
  for (const args of [{}, { destination_id: "d1", grant_id: "dpq_abcdefgh" }, { destination_id: "d1", label: "forged" }]) {
    const refused = await runCall(ctx(), "publish.destination.use", args as Record<string, unknown>, ui);
    assert(!refused.ok && refused.refusal.code === "bad_args", "bad args refused");
  }
  // No existing binding: create with a request id, no revision.
  replies["/contexts/ctx1/bindings"] = { body: { bindings: [] } };
  calls.length = 0;
  const fresh = await runCall(ctx(), "publish.destination.use", { destination_id: "d1" }, ui);
  assert(fresh.ok, "creates the binding");
  const sent = calls[calls.length - 1].body as Record<string, unknown>;
  assert(typeof sent.request_id === "string" && !("expected_revision" in sent), "create carries a request id and no revision");

  // CAD-1918 (operator decision, reversing CAD-1905's slot-only approvals): the app's own
  // confirm and schedule, and the follow-ups to an approved post (reschedule, cancel), are
  // direct calls. The app's own press is the one approval; the host-held token still binds it.
  const token = "ab".repeat(32);
  const effect = "sfx_install_1";
  const due = Math.floor(Date.now() / 1000) + 3600;
  replies = { "/api/app-social-drafts/confirm": { body: { effect: { state: "posted" } } }, "/api/app-social-drafts/schedule": { body: { ok: 1 } },
    "/api/app-social-drafts/reschedule": { body: { ok: 2 } }, "/api/app-social-drafts/cancel": { body: { ok: 3 } } };
  let reloads = 0;
  const direct = (verb: string, args: Record<string, unknown>, actionToken: string | null = token) =>
    runCall(ctx(() => { reloads += 1; }), verb as Parameters<typeof runCall>[1], args, { ...ui, actionToken: actionToken ?? undefined });
  calls.length = 0;
  for (const [verb, args, path] of [
    ["publish.draft.confirm", { alias: "social.draft", effect_id: effect }, "confirm"],
    ["publish.draft.schedule", { alias: "social.draft", effect_id: effect, due_epoch: due }, "schedule"],
    ["publish.draft.reschedule", { alias: "social.draft", effect_id: effect, due_epoch: due + 60 }, "reschedule"],
    ["publish.draft.cancel", { alias: "social.draft", effect_id: effect }, "cancel"],
  ] as const) {
    const done = await direct(verb, { ...args });
    assert(done.ok, `${verb} runs directly`);
    const sentCall = calls[calls.length - 1];
    assert(sentCall.path.endsWith(`/api/app-social-drafts/${path}`), `${verb} reaches its daemon route`);
    const body = sentCall.body as Record<string, unknown>;
    equal(body.tool_alias, "social.draft", "alias is host-mapped");
    equal(body.action_token, token, "the host-held mount token is sent");
    equal(body.effect_id, effect, "only the named effect travels");
    assert(!("digest" in body) && !("revision" in body), "no frame-supplied digest or revision");
  }
  equal(reloads, 4, "each action reloads the board");
  const before = calls.length;
  assert(!(await direct("publish.draft.reschedule", { alias: "social.draft", effect_id: effect, due_epoch: due }, null)).ok, "no mount token: refused before any request");
  assert(!(await direct("publish.draft.reschedule", { alias: "social.draft", effect_id: effect, due_epoch: due, digest: "sha256:forged" })).ok, "an extra field is refused");
  assert(!(await direct("publish.draft.reschedule", { alias: "social.draft", effect_id: effect, due_epoch: 1 })).ok, "a past time is refused");
  assert(!(await direct("publish.draft.cancel", { alias: "social.draft", effect_id: "../x" })).ok, "a malformed effect id is refused");
  assert(!(await direct("publish.draft.confirm", { alias: "social.draft", effect_id: effect }, null)).ok, "confirm with no mount token: refused before any request");
  equal(calls.length, before, "refusals make no daemon request");
  const asSlot = await makePlanner(() => ctx())("publish.draft.schedule", { alias: "social.draft", effect_id: effect, due_epoch: due }, { actionToken: token });
  assert(!("code" in asSlot), "an older app's schedule slot still plans (transition)");
}

main().then(() => console.log("socialConnect ok"));
