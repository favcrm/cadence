// CAD-1123 HP1: screen.v2 read projection and the image channel, through
// the real ScreenChannel a mounted frame talks to. Fixtures mirror the
// daemon's app_run_show / installation / binding / receipt JSON.
import { screenProjection, type ScreenExtras } from "../src/features/workspace-apps/screen/screenProjection";
import { pushBytes, PUSH_BYTES_MAX, type ScreenPush } from "../src/features/workspace-apps/screen/screenProtocol";
import { ScreenChannel, ASSET_QUEUE_MAX } from "../src/features/workspace-apps/screen/screenLifecycle";
import { toPublishIntent, type PublishIntent } from "../src/features/workspace-apps/socialPublish";
import type { AppBinding, AppContext, AppEffect, Installation, SourceReceipt, WorkspaceRun } from "../src/features/workspace-apps/workspaceApps";

function check(value: unknown, label: string): void { if (!value) throw new Error(label); }
const sha = (c: string) => `sha256:${c.repeat(64)}`;
const keys = (value: object) => Object.keys(value).sort().join();
// Secret-shaped markers are built at runtime and planted in every private field.
const SECRET = ["PRIV", "ATE"].join("");
const slot = (effect: string, capability: string) => ({ schema: 1, capability, version: 1, action: "a", resource_kind: "connection_account", effect });
const installation = {
  install_id: "inst-1", digest: sha("b"), name: "any-app", title: "Any app", version: "1.0.0", summary: "s",
  approved: true, executable: true, files: ["screens/main/screens.json", "workflows/draft.md", "workflows/read.md"],
  capabilities: { source: slot("read", "social.read"), image: slot("draft", "media.generate"), publication: slot("send", "text.publish") },
  workflows: [
    { name: "draft", source_digest: sha("a"), inputs: [
      { name: "content_prompt", context_default: true, default: "Write briefly." },
      { name: "image_prompt", context_default: true, default: "One photo." },
      { name: "writer", context_default: false } ] },
    { name: "read", source_digest: sha("f"), inputs: [{ name: "profile_handle", context_default: false }] },
  ],
} as unknown as Installation;
const context = { id: "ctx-a", install_id: "inst-1", revision: 4, digest: sha("c"), state: "active",
  config: { schema: 1, label: "Brand A", input_defaults: { content_prompt: "Warm and short.", brand_voice: `${SECRET} voice` } } } as AppContext;
const otherContext = { ...context, id: "ctx-b", config: { ...context.config, label: "Brand B", input_defaults: {} } } as AppContext;
const quote = (micros: number) => ({ schema: 1 as const, currency: "USD" as const, unit_price_micros: micros, units: 1, total_price_micros: micros, price_revision: "p1" });
const run = (over: Partial<WorkspaceRun> & { id: string }): WorkspaceRun => ({
  install_id: "inst-1", context_id: "ctx-a", state: "succeeded", snapshot_digest: sha("5"), approved_digest: sha("5"),
  created: 1_790_000_000, updated: 1_790_000_100, approval: { by: "operator", at: 1_790_000_010 },
  snapshot: {
    workflow: { title: "Draft", source_digest: sha("a"), steps: [
      { id: "s1", kind: "produce_text", assignee: "writer", dependencies: [], instruction: `${SECRET} instruction` },
      { id: "s2", kind: "review_text", assignee: "reviewer", dependencies: ["s1"], instruction: "r" }] },
    inputs: { content_prompt: "Warm and short.", image_prompt: "Per-post idea", writer: `${SECRET}-writer`, source: `${SECRET} facts` },
    input_origins: { content_prompt: "context_default", image_prompt: "run_override" },
    owner_pm: `${SECRET}-pm`, assignments: { s1: { alias: `${SECRET}-alias`, role: "worker", provider: "pi" } },
    capabilities: { image: { id: `${SECRET}-binding`, revision: 1, digest: sha("d") } },
    quotes: { image: quote(60000) },
    source: { receipt_id: `${SECRET}-source-receipt`, post: { id: "post-1", caption: `${SECRET} caption` } },
  },
  steps: [{ step_id: "s1", task_id: `${SECRET}-task`, state: "succeeded", message_id: `${SECRET}-msg` },
    { step_id: "s2", task_id: "t2", state: "succeeded", message_id: "m2" }],
  artifacts: [{ id: "art-1", step_id: "s1", digest: sha("7"), media_type: "text/markdown", size: 900 }],
  reviews: [{ step_id: "s2", artifact_digest: sha("7"), reviewer: `${SECRET}-reviewer`, decision: "approve",
    rationale: "Grounded in the source.", asset_receipt_id: `${SECRET}-asset-receipt`, asset_digest: sha("1") }],
  ...over,
} as WorkspaceRun);
const readRun = run({ id: "run-read", snapshot: { ...run({ id: "x" }).snapshot, workflow: { title: "Read", source_digest: sha("f"),
  steps: [{ id: "s1", kind: "produce_text", assignee: "w", dependencies: [], instruction: "i" }] },
  capabilities: { source: { id: "b", revision: 1, digest: sha("e") } }, quotes: { source: quote(10000) }, source: null },
  artifacts: [], reviews: [], updated: 1_790_000_500 });
const runs: WorkspaceRun[] = [
  run({ id: "run-ready" }),
  run({ id: "run-writing", state: "running", approval: undefined, reviews: [], artifacts: [],
    steps: [{ step_id: "s1", task_id: "t", state: "dispatched", message_id: null }, { step_id: "s2", task_id: "t", state: "pending", message_id: null }] }),
  run({ id: "run-checking", state: "running", reviews: [],
    steps: [{ step_id: "s1", task_id: "t", state: "succeeded", message_id: null }, { step_id: "s2", task_id: "t", state: "dispatched", message_id: null }] }),
  run({ id: "run-failed", state: "failed", reviews: [{ step_id: "s2", artifact_digest: sha("7"), reviewer: "r", decision: "revise", rationale: "The caption names a price the source never states." }] }),
  readRun,
  // Same install, other context; and another install entirely.
  run({ id: "run-other-context", context_id: "ctx-b" }),
  run({ id: "run-other-install", install_id: "inst-2" }),
];
const effects: AppEffect[] = [];
const intent = (over: Partial<PublishIntent>): PublishIntent => ({ intent_id: "i1", install_id: "inst-1", context_id: "ctx-a",
  run_id: "run-ready", effect_id: "e1", state: "posted", channel: "instagram", destination_id: "dest", caption_digest: "c",
  image_digest: null, frozen_digest: "f", idempotency_key: `${SECRET}-key`, due_epoch: 1_790_001_000, timezone: "Asia/Hong_Kong",
  grant_id: `${SECRET}-grant`, approval_id: `apv-${SECRET}`, writer: null, reviewer: null,
  permalink: "https://www.instagram.com/p/abc/", receipt: { token: `${SECRET}-receipt` }, refusal: null, upstream: { k: SECRET }, ...over });
const daemonIntent = (intent_id: string, state: string, receipt: Record<string, unknown>): PublishIntent => toPublishIntent({
  intent_id, request: `${SECRET}-request`, state, frozen_digest: `${SECRET}-frozen`, upstream: null, receipt,
  frozen: { install_id: "inst-1", context_id: "ctx-a", run_id: "run-ready", effect_id: "e1", artifact_id: "art-1",
    toolkit: "instagram", destination_id: "dest", caption_digest: "c".repeat(64), image_digest: null, due_epoch: 1_790_001_000,
    timezone: "Asia/Hong_Kong", grant_id: `dpq_${SECRET}grant`, approval_id: `apv-${SECRET}` } } as never);
const intents = { installId: "inst-1", contextId: "ctx-a", status: "ok" as const, intents: [
  intent({}),
  // Refusals as the daemon stores them (receipt.error = "<code>: <detail>",
  // receipt.reason for a hold), mapped by the board's real toPublishIntent.
  daemonIntent("i2", "refused", { error: `provider_refused: door provider_refused: (#9004) The media could not be fetched from https://media.example/u/abc?X-Amz-Signature=${SECRET}SIGNED fbtrace_id=${SECRET}TRACE` }),
  daemonIntent("i5", "refused", { error: `refused: posted report failed: sqlite error: database is locked at /tmp/${SECRET}/state.db` }),
  daemonIntent("i6", "held", { reason: "missed publish window" }),
  daemonIntent("i7", "held", { reason: `the door has no record of this publish ${SECRET}` }),
  daemonIntent("i8", "refused", { error: `Some Provider Text ${SECRET}` }),
  intent({ intent_id: "i3", permalink: "javascript:alert(1)" }),
  intent({ intent_id: "i4", permalink: "https://evil.example/p/abc/" }),
  intent({ intent_id: "i-foreign-context", context_id: "ctx-b", run_id: "run-other-context" }),
] };
const binding = (slotName: string, context_id: string | null): AppBinding => ({ id: `${SECRET}-binding-${slotName}`, install_id: "inst-1",
  context_id, slot: slotName, revision: 1, state: "configured", digest: "d",
  config: { bundle_digest: sha("b"), connection_id: `${SECRET}-conn`, provider: "p", account: `${SECRET}-account`, mapping: {} as never } });
const receipt: SourceReceipt = { id: `${SECRET}-receipt-id`, run_id: "run-read", slot: "source", digest: "d", binding_digest: "b",
  result: { schema: 1, kind: "social.source.posts", provider: "agenticos_external", source_tool: "read_instagram_posts", handle: "sakeboyhk",
    profile_verified: true, empty_reason: null, more_available: false, posts: [
      { id: "post-1", caption: "Line one", permalink: "https://www.instagram.com/p/one/", published_at: null, published_at_unix: 1_789_000_000,
        media_kind: "image", preview_url: "https://scontent-hkg1-1.cdninstagram.com/v/t51/one.jpg?oe=1" },
      { id: "post-2", caption: "x", permalink: "http://www.instagram.com/p/two/", published_at: null, published_at_unix: null,
        media_kind: "carousel", preview_url: null },
      { id: "post-3", caption: "x".repeat(900), permalink: "https://instagram.com/p/three/", published_at: null, published_at_unix: 1_789_000_100,
        media_kind: "carousel", preview_url: "https://evil.example/two.jpg" }] } };
const extras = (over: Partial<ScreenExtras> = {}): ScreenExtras => ({
  installId: "inst-1", contextId: "ctx-a", bindings: [binding("source", "ctx-a"), binding("image", "ctx-a"), binding("image", "ctx-b")],
  workers: 2,
  source: { runId: "run-read", receipts: [receipt] }, texts: new Map([["art-1", "我哋今日開咗新酒。" + "y".repeat(800)]]),
  ...over });
const project = (ext: ScreenExtras | undefined = extras()) =>
  screenProjection(installation, "main", "ctx-a", [context, otherContext], runs, effects, intents, ext);

// --- the real channel a mounted frame talks to ---
class Port {
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onmessageerror: (() => void) | null = null;
  sent: unknown[] = []; closed = false;
  start() {} close() { this.closed = true; }
  postMessage(value: unknown) { this.sent.push(value); }
  receive(value: unknown) { this.onmessage?.({ data: value }); }
}
const nonce = "a".repeat(64);
const receiptMount = { mount: `/api/app-screen/${"b".repeat(64)}`, bridge_nonce: nonce, generation: 1, tag: "main" };
function mount(push: ScreenPush, loader = async (_ref: string): Promise<string | null> => null) {
  const source = {} as Window; const port = new Port(); let failed = 0; const asked: string[] = [];
  const channel = new ScreenChannel(source, receiptMount, push, () => {}, () => ++failed, () => {},
    async ref => { asked.push(ref); return loader(ref); });
  channel.receive({ source, origin: "null", data: { v: 1, op: "init", tag: "main", bridge_nonce: nonce, generation: 1 }, ports: [port as unknown as MessagePort] });
  return { channel, port, failed: () => failed, asked };
}
const tick = () => new Promise(resolve => setTimeout(resolve, 0));

void (async () => {
  const full = project();

  // 1. A v1 child receives exactly the CAD-1006 v1 shape, v2 extras or not.
  const v1 = mount(full); v1.port.receive({ v: 1, op: "ready" });
  const got = v1.port.sent[0] as ScreenPush;
  check(keys(got) === "context_id,contexts,digest,install_id,installation,now,op,outbox,runs,tag,v" && got.v === 1, `v1 top-level keys: ${keys(got)}`);
  check(got.runs.length === 5 && got.runs.every(r => keys(r) === "id,snapshot_digest,state,title,workflow" && keys(r.workflow) === "title"),
    "v1 run rows are exactly id/state/title/snapshot_digest/workflow{title}");
  const v1Bare = mount(project(undefined)); v1Bare.port.receive({ v: 1, op: "ready" });
  const bare = v1Bare.port.sent[0] as ScreenPush;
  check(JSON.stringify({ ...got, now: 0 }) === JSON.stringify({ ...bare, now: 0 }), "v1 push is identical with and without v2 board reads");
  const intentsChild = mount(full); intentsChild.port.receive({ v: 1, op: "ready", accepts: ["publish-intents.v1"] });
  const ip = intentsChild.port.sent[0] as ScreenPush;
  check(ip.v === 1 && !("sources" in ip) && !("actions" in ip) && ip.runs.every(r => !("phase" in r) && !("created" in r) && keys(r.workflow) === "title") &&
    ip.publish_intents!.rows.every(r => !("refusal" in r) && !("permalink" in r)), "a publish-intents.v1 child keeps its exact shape");

  // 2. A v2 child receives phase, caption excerpt, refusal, permalink and defaults (no price).
  const v2 = mount(full); v2.port.receive({ v: 1, op: "ready", accepts: ["screen.v2"] });
  const p = v2.port.sent[0] as ScreenPush;
  check(p.v === 2 && p.actions!.join() === "read.run,context.defaults.save,open-link,run.start" && !!p.publish_intents, "v2 envelope: v 2, intents kept, the closed verb list for an operator view");
  const byId = new Map(p.runs.map(r => [r.id, r]));
  check(byId.get("run-ready")!.phase === "ready" && byId.get("run-writing")!.phase === "working:produce_text" &&
    byId.get("run-checking")!.phase === "checking" && byId.get("run-failed")!.phase === "failed", "phase from real step receipts");
  const ready = byId.get("run-ready")!;
  check(ready.caption_excerpt!.startsWith("我哋今日開咗新酒。") && ready.caption_excerpt!.length === 600 && ready.artifact_id === "art-1",
    "caption excerpt is the approved artifact text, ≤ 600 chars");
  check(!("caption_excerpt" in byId.get("run-writing")!), "no excerpt before a review approves the text");
  check(ready.workflow.name === "draft" && ready.workflow.kind === "draft" && byId.get("run-read")!.workflow.kind === "read",
    "workflow name from the installed digest, kind from declared slot effects");
  check(!("price" in ready) && !("prices" in p) && !/"(price|prices|amount|currency|total_price_micros|unit_price_micros|price_revision)"/.test(JSON.stringify(p)),
    "no cost or price is pushed to the frame (CAD-1129 #8)");
  check(ready.created === 1_790_000_000 && ready.closed === 1_790_000_100 && !("closed" in byId.get("run-writing")!), "run epochs");
  check(ready.approved!.by_display === "Operator" && ready.approved!.at === 1_790_000_010 && !("approved" in byId.get("run-writing")!),
    "approval display and time from the recorded approval");
  check(ready.source_post_id === "post-1" && ready.image_ref === "image:run-ready", "source post and host-minted image ref");
  check(keys(ready.inputs_used!) === "content_prompt,image_prompt" && ready.inputs_used!.content_prompt.origin === "context_default",
    "inputs_used carries only context_default keys, with origin");
  check(byId.get("run-failed")!.refusal!.code === "review_revise" && byId.get("run-failed")!.refusal!.text.includes("never states"),
    "run refusal is a plain line");
  check(!("refusal" in byId.get("run-ready")!), "a ready run has no refusal");
  const rows = new Map(p.publish_intents!.rows.map(r => [r.intent_id, r]));
  check(rows.get("i1")!.permalink === "https://www.instagram.com/p/abc/", "posted permalink pushed");
  const refusal = (id: string) => JSON.stringify(rows.get(id)!.refusal);
  check(refusal("i2") === JSON.stringify({ code: "provider_refused", text: "Instagram refused the post. Nothing was published." }),
    `a provider refusal keeps its code and gets host copy, never the provider text: ${refusal("i2")}`);
  check(refusal("i5") === JSON.stringify({ code: "refused", text: "The post was refused. Nothing was published." }),
    `a store failure is the generic refusal: ${refusal("i5")}`);
  check(refusal("i6") === JSON.stringify({ code: "missed_window", text: "The scheduled time passed before it could be sent. Nothing was sent." }),
    `a known hold gets host copy: ${refusal("i6")}`);
  check(refusal("i7") === JSON.stringify({ code: "held", text: "Held — nothing was sent." }), `an unknown hold is generic: ${refusal("i7")}`);
  check(refusal("i8") === JSON.stringify({ code: "refused", text: "The post was refused. Nothing was published." }),
    `an uncoded error is generic: ${refusal("i8")}`);
  for (const leak of ["X-Amz", "fbtrace", "sqlite", "media.example", "door", "Provider Text"])
    check(!JSON.stringify(p).includes(leak), `daemon detail reached the frame: ${leak}`);
  check(!("permalink" in rows.get("i3")!) && !("permalink" in rows.get("i4")!) && !("refusal" in rows.get("i1")!),
    "non-https or non-Instagram permalinks are dropped; absent fields are omitted");
  check(p.defaults!.context_id === "ctx-a" && p.defaults!.revision === 4 && p.defaults!.values.content_prompt === "Warm and short." &&
    p.defaults!.values.image_prompt === "One photo." && keys(p.defaults!) === "context_id,revision,values" &&
    keys(p.defaults!.values) === "content_prompt,image_prompt",
    "defaults: context value over app default, context_default keys only");
  check(p.sources!.handle === "sakeboyhk" && p.sources!.run_id === "run-read" &&
    p.sources!.fetched_at === 1_790_000_500 && p.sources!.posts.length === 2, "sources from the newest read run");
  const [post1, post2, post3] = p.sources!.posts;
  check(keys(post1) === "caption,id,media_kind,permalink,published_at,thumb_url" && post1.thumb_url!.includes("cdninstagram.com") &&
    post1.permalink === "https://www.instagram.com/p/one/" && post1.published_at === 1_789_000_000, "source post fields");
  check(post2.id === "post-3" && !("thumb_url" in post2) && post2.caption.length === 600 && post3 === undefined,
    "off-host thumb dropped, caption bounded, a post without a valid link and time withheld");
  check(p.readiness!.ok === false && p.readiness!.blockers.join() === "slot_unbound.publication", "readiness names the real blocker");

  // 3. Rows of another install or context are withheld.
  check(!byId.has("run-other-context") && !byId.has("run-other-install") && !rows.has("i-foreign-context"),
    "foreign runs and intents withheld");
  check(p.contexts.map(c => c.id).join() === "ctx-a,ctx-b" && p.context_id === "ctx-a", "contexts list is labels only");
  const foreignExtras = screenProjection(installation, "main", "ctx-a", [context], runs, effects, intents,
    extras({ contextId: "ctx-b" }));
  check(!("sources" in foreignExtras) && foreignExtras.actions!.length === 0 &&
    foreignExtras.runs.every(r => !("caption_excerpt" in r)), "board reads tagged for another context are never pushed");
  check(!("sources" in screenProjection(installation, "main", "ctx-a", [context], runs, effects, intents, extras({ installId: "inst-2" }))),
    "board reads tagged for another install are never pushed");
  const otherScope = screenProjection(installation, "main", "ctx-b", [context, otherContext], runs, effects, undefined,
    extras({ contextId: "ctx-b" }));
  check(!("sources" in otherScope) && otherScope.defaults!.context_id === "ctx-b" &&
    otherScope.defaults!.values.content_prompt === "Write briefly.", "a read run of another context never fills this library");

  // 4. No grant, approval id, media key, receipt or binding ever appears.
  const wire = JSON.stringify(p);
  check(!wire.includes(SECRET), `a private field reached the frame: ${wire.slice(Math.max(0, wire.indexOf(SECRET) - 80), wire.indexOf(SECRET) + 40)}`);
  for (const banned of ["grant", "approval_id", "idempotency", "receipt", "asset_receipt", "binding", "assignments", "owner_pm", "apv-"])
    check(!wire.includes(banned), `banned field name on the wire: ${banned}`);

  // 5. An oversized projection degrades by section, never dropping the screen.
  const many = Array.from({ length: 120 }, (_, n) => run({ id: `run-big-${n}`, artifacts: [{ id: `art-big-${n}`, step_id: "s1", digest: sha("7"), media_type: "text/markdown", size: 1 }] }));
  const big = screenProjection(installation, "main", "ctx-a", [context], [...many, readRun], effects, intents,
    extras({ texts: new Map(many.map((_, n) => [`art-big-${n}`, "字".repeat(600)])) }));
  check(pushBytes(big) > PUSH_BYTES_MAX, "fixture: full v2 projection is over the cap");
  const bigChild = mount(big); bigChild.port.receive({ v: 1, op: "ready", accepts: ["screen.v2"] });
  const degraded = bigChild.port.sent[0] as ScreenPush;
  check(!bigChild.port.closed && pushBytes(degraded) <= PUSH_BYTES_MAX, "degraded push is sent within the cap");
  check(degraded.v === 2 && degraded.runs.every(r => !("caption_excerpt" in r)) &&
    degraded.runs.length === 121 && degraded.runs[0].phase === "ready", "excerpts go first; runs and phases stay");
  check(!!degraded.sources && !!degraded.defaults && !!degraded.readiness, "later sections stay while they fit");

  // 6. Image channel: only a pushed ref, only for a v2 child; unknown refs close the port.
  const dataUrl = "data:image/jpeg;base64," + "A".repeat(64);
  const served = mount(full, async ref => ref === "image:run-ready" ? dataUrl : null);
  served.port.receive({ v: 1, op: "ready", accepts: ["screen.v2"] });
  served.port.receive({ v: 2, op: "asset", ref: "image:run-ready" }); await tick();
  const reply = served.port.sent[1] as Record<string, unknown>;
  check(keys(reply) === "data_url,op,ref,v" && reply.v === 2 && reply.data_url === dataUrl && !served.port.closed, "pushed ref is served");
  const v1Ask = mount(full, async () => dataUrl); v1Ask.port.receive({ v: 1, op: "ready", accepts: ["screen.v2"] });
  v1Ask.port.receive({ v: 1, op: "asset", ref: "image:run-ready" });
  check(v1Ask.port.closed && v1Ask.asked.length === 0, "an asset request must be v 2");
  const unknown = mount(full, async () => dataUrl); unknown.port.receive({ v: 1, op: "ready", accepts: ["screen.v2"] });
  unknown.port.receive({ v: 2, op: "asset", ref: "image:run-other-install" }); await tick();
  check(unknown.port.closed && unknown.failed() === 1 && unknown.asked.length === 0, "a ref that was not pushed is refused and the port closes");
  const writing = mount(full, async () => dataUrl); writing.port.receive({ v: 1, op: "ready", accepts: ["screen.v2"] });
  writing.port.receive({ v: 2, op: "asset", ref: "image:run-writing" });
  check(writing.port.closed, "a pushed run without a reviewed image has no ref to ask for");
  const legacyAsk = mount(full, async () => dataUrl); legacyAsk.port.receive({ v: 1, op: "ready", accepts: ["publish-intents.v1"] });
  legacyAsk.port.receive({ v: 2, op: "asset", ref: "image:run-ready" });
  check(legacyAsk.port.closed && legacyAsk.asked.length === 0, "a child without screen.v2 cannot use the channel");
  const early = mount(full, async () => dataUrl); early.port.receive({ v: 2, op: "asset", ref: "image:run-ready" });
  check(early.port.closed, "no asset before ready");
  for (const forged of [{ v: 2, op: "asset", ref: "art-1" }, { v: 2, op: "asset", ref: "image:run-ready", receipt: "x" },
    { v: 2, op: "asset", ref: "image:../x" }]) {
    const f = mount(full, async () => dataUrl); f.port.receive({ v: 1, op: "ready", accepts: ["screen.v2"] }); f.port.receive(forged);
    check(f.port.closed && f.asked.length === 0, `forged asset request refused: ${JSON.stringify(forged)}`);
  }
  for (const bad of ["data:text/html;base64,AAAA", "https://scontent.cdninstagram.com/x.jpg", "data:image/jpeg;base64," + "A".repeat(96 * 1024), null]) {
    const b = mount(full, async () => bad); b.port.receive({ v: 1, op: "ready", accepts: ["screen.v2"] });
    b.port.receive({ v: 2, op: "asset", ref: "image:run-ready" }); await tick();
    check(b.port.sent.length === 1 && !b.port.closed, `an off-contract, oversize or failed image is never relayed: ${String(bad).slice(0, 30)}`);
  }
  // A repeated request while one is in flight gets exactly one reply.
  const twice = mount(full, async () => dataUrl); twice.port.receive({ v: 1, op: "ready", accepts: ["screen.v2"] });
  twice.port.receive({ v: 2, op: "asset", ref: "image:run-ready" }); twice.port.receive({ v: 2, op: "asset", ref: "image:run-ready" }); await tick();
  check(twice.port.sent.length === 2 && twice.asked.length === 1, "in-flight requests are deduplicated");
  // A flood of requests loads four at a time; repeats are deduplicated.
  // Pushed refs are bounded by the 256-run cap, which matches the queue cap.
  const floodRuns = Array.from({ length: 256 }, (_, n) => run({ id: `f${n}` }));
  const flood = mount(screenProjection(installation, "main", "ctx-a", [context], floodRuns, effects, intents, extras()), () => new Promise(() => {}));
  flood.port.receive({ v: 1, op: "ready", accepts: ["screen.v2"] });
  check(!flood.port.closed && ASSET_QUEUE_MAX === 256, "fixture: a full projection is sent");
  for (let n = 0; n < 2 * ASSET_QUEUE_MAX; n++) flood.port.receive({ v: 2, op: "asset", ref: `image:f${n % 256}` });
  check(!flood.port.closed && flood.asked.length === 4, "outstanding requests load four at a time");
  // Opt-in grammar.
  const both = mount(full); both.port.receive({ v: 1, op: "ready", accepts: ["publish-intents.v1", "screen.v2"] });
  check("sources" in (both.port.sent[0] as object), "listing both opt-ins negotiates v2");
  for (const accepts of [["screen.v2", "screen.v2"], ["screen.v3"], ["screen.v2", "grants"], ["screen.v2", "publish-intents.v1", "x"]]) {
    const f = mount(full); f.port.receive({ v: 1, op: "ready", accepts });
    check(f.port.closed, `forged opt-in refused: ${JSON.stringify(accepts)}`);
  }
  console.log("screen.v2 projection and image channel checks pass");
})().catch(error => setTimeout(() => { throw error; }));
