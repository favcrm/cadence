import { ApiError } from "../src/lib/api";
import { subscribeContext } from "../src/features/workspace-apps/contextSelection";
import { plainRefusal, runCall, type ActionContext } from "../src/features/workspace-apps/screen/screenActions";
import type { Installation } from "../src/features/workspace-apps/workspaceApps";

function assert(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }
function equal(actual: unknown, expected: unknown, why: string) {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) throw new Error(`${why}: ${JSON.stringify(actual)} !== ${JSON.stringify(expected)}`);
}

const events: string[] = [];
const n = (list: unknown[]) => list.length;
const ui = (shown: string[]) => ({ showLink: (url: string) => { shown.push(url); }, navigate: (path: string) => { events.push(`navigate ${path}`); } });

async function main() {
  // CAD-1288 (1): a board path moves the router; an external link is only shown for the confirm.
  const shown: string[] = [];
  const ctx = {} as ActionContext;
  const board = await runCall(ctx, "open-link", { url: "/settings/connections" }, ui(shown));
  assert(board.ok && events.join() === "navigate /settings/connections" && n(shown) === 0, "board path navigates, shows no pill");
  const external = await runCall(ctx, "open-link", { url: "https://www.instagram.com/p/abc/" }, ui(shown));
  assert(external.ok && shown.join() === "https://www.instagram.com/p/abc/" && n(events) === 1, "external link is shown for confirm, never navigated");
  for (const url of ["javascript:alert(1)", "http://www.instagram.com/p/abc/", "https://evil.example/x", "//evil.example/x", "/settings/other", "/settings/connections?x=1", "data:text/html,x"]) {
    const refused = await runCall(ctx, "open-link", { url }, ui(shown));
    assert(!refused.ok && refused.refusal.code === "link_blocked", `refused: ${url}`);
  }
  assert(n(events) === 1 && n(shown) === 1, "refused links neither navigate nor show");

  // CAD-1288 (2): the new context becomes the selection only after the board has re-read it,
  // so the screen remounts once with the context its tool session needs.
  const store = new Map<string, string>();
  Object.defineProperty(globalThis, "sessionStorage", { configurable: true, value: {
    getItem: (key: string) => store.get(key) ?? null, setItem: (key: string, value: string) => store.set(key, value),
    removeItem: (key: string) => store.delete(key) } });
  Object.defineProperty(globalThis, "window", { configurable: true, value: { sessionStorage: (globalThis as { sessionStorage: Storage }).sessionStorage } });
  globalThis.fetch = async (input, init) => {
    const body = String(input).endsWith("/contexts") && init?.method === "POST"
      ? { context: { id: "ctx-new", install_id: "inst", revision: 1, digest: "d", state: "active", config: { schema: 1, label: "Default", input_defaults: { handle: "x" } } } }
      : { contexts: [] };
    return new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } });
  };
  const installation = { workflows: [{ name: "w", inputs: [{ name: "handle", context_default: true }] }] } as unknown as Installation;
  const order: string[] = [];
  const unsubscribe = subscribeContext((_, id) => { order.push(`select ${id}`); });
  const saved = await runCall({ installation, installId: "inst", contextId: "", runs: [],
    onChanged: async () => { await new Promise(resolve => setTimeout(resolve, 5)); order.push("reread"); } },
    "context.defaults.save", { values: { handle: "x" }, expected_revision: 0 }, ui([]));
  unsubscribe();
  assert(saved.ok, "first save creates the Default context");
  equal(order, ["reread", "select ctx-new"], "board re-reads before the new context is selected");

  // CAD-1288 (3): a missing binding reaches the app as a typed setup reason.
  equal(plainRefusal(new ApiError("tool capability binding is absent", 409)).code, "binding_absent", "binding_absent code");
  equal(plainRefusal(new ApiError("something else", 409)).code, "refused", "other refusals stay generic");
  // CAD-1303: discard sends exactly {draft_id, revision} (the host names the draft alias) and
  // names the host's refusal codes; an extra field such as an alias never leaves the frame.
  const sent: { url: string; body: Record<string, unknown> }[] = [];
  globalThis.fetch = async (input, init) => {
    sent.push({ url: String(input), body: JSON.parse(String(init?.body ?? "{}")) });
    return new Response(JSON.stringify({ draft_id: "sdr-1", state: "discarded", revision: 3 }), { status: 200, headers: { "Content-Type": "application/json" } });
  };
  const withToken = { ...ui([]), actionToken: "a".repeat(64) };
  const discarded = await runCall(ctx, "social.drafts.discard", { draft_id: "sdr-1", revision: 3 }, withToken);
  equal(discarded.ok && (discarded.data as Record<string, unknown>).state, "discarded", "discard relays the host reply");
  assert(sent.length === 1 && sent[0].url.endsWith("/api/app-social-drafts/discard"), "discard uses its own route");
  equal(Object.keys(sent[0].body).sort(), ["action_token", "draft_id", "revision", "tool_alias"], "discard body fields");
  for (const bad of [{ alias: "social.draft", draft_id: "sdr-1", revision: 3 }, { draft_id: "sdr-1" }, { draft_id: "sdr-1", revision: 1.5 }]) {
    const refused = await runCall(ctx, "social.drafts.discard", bad, withToken);
    assert(!refused.ok && refused.refusal.code === "bad_args", `discard refuses ${JSON.stringify(bad)}`);
  }
  assert(sent.length === 1, "refused discards never reach the host");
  equal(plainRefusal(new ApiError("x", 400, { code: "draft_in_use" })).code, "draft_in_use", "draft_in_use code");
  equal(plainRefusal(new ApiError("revision is stale", 409, { code: "stale_revision" })).code, "stale_revision", "stale_revision code");
  console.log("screenActionsCad1288 ok");
}
main().catch(error => { console.error(error); { throw error; }; });
