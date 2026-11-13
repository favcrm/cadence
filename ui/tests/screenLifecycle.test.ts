import { parseChild, parseDraftImageRef, parseInit, parseScreenAudioCapture, SCREEN_AUDIO_V1, SCREEN_LOCALE_V1, type ScreenPush } from "../src/features/workspace-apps/screen/screenProtocol";
import { parseMount, ScreenChannel } from "../src/features/workspace-apps/screen/screenLifecycle";

function check(value: unknown, label: string): void { if (!value) throw new Error(label); }
const nonce = "a".repeat(64);
const receipt = { mount: `/api/app-screen/${"b".repeat(64)}`, bridge_nonce: nonce, generation: 2, tag: "main" };
const init = { v: 1, op: "init", tag: "main", bridge_nonce: nonce, generation: 2 };
const push: ScreenPush = { v: 1, op: "screen", install_id: "i", digest: "d", tag: "main", context_id: "c",
  installation: { install_id: "i", digest: "d", title: "t", name: "n", version: "v", summary: "" },
  contexts: [{ id: "c", label: "context" }], runs: [], outbox: [], now: 1 };
check(parseMount(receipt, "main"), "valid distinct capability/nonce");
check(!parseMount({ ...receipt, mount: `/api/app-screen/${nonce}` }, "main"), "capability is not bridge nonce");
check(!parseMount({ ...receipt, mount: "https://other.example/token" }, "main"), "foreign frame URL refused");
check(!parseInit({ ...init, operator: true }), "forged init field refused");
check(!parseChild({ v: 1, op: "ready", actor: "operator" }), "forged port field refused");
check(!parseChild({ v: 1, op: "state", data: "界".repeat(12000) }), "UTF8 state bound");
check(parseChild({ v: 1, op: "state", data: "x".repeat(32768) }), "exact byte bound accepted");
check(parseChild({v:2,op:"tool",id:"t1",alias:"text.generate",input:{messages:[]},request_id:"request-a",generation_scope:{operation:"caption",draft_id:"draft-a",revision:2}}),"draft-revision generation intent parses");
check(!parseChild({v:2,op:"tool",id:"t1",alias:"text.generate",input:{messages:[]},request_id:"request-a",generation_scope:{operation:"caption",draft_id:"draft-a"}}),"generation intent without revision is refused");
check(!parseChild({v:2,op:"tool",id:"t1",alias:"text.generate",input:{messages:[]},request_id:"request-a",generation_scope:{operation:"caption",draft_id:"draft-a",revision:2,request_id:"other"}}),"generation intent cannot override its key");

class Port {
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onmessageerror: (() => void) | null = null;
  sent: unknown[] = [];
  closed = false;
  start() {}
  close() { this.closed = true; }
  postMessage(value: unknown) { this.sent.push(value); }
  receive(value: unknown) { this.onmessage?.({ data: value }); }
}
function mount() {
  const source = {} as Window;
  const port = new Port();
  let removed = 0, failed = 0, ready = 0;
  const channel = new ScreenChannel(source, receipt, push, () => ++removed, () => ++failed, () => ++ready);
  const event = { source, origin: "null", data: init, ports: [port as unknown as MessagePort] };
  return { channel, port, event, counts: () => ({ removed, failed, ready }) };
}
const good = mount();
good.channel.receive(good.event);
check(good.port.sent.length === 0, "no private push before ready");
good.port.receive({ v: 1, op: "ready" });
check(good.port.sent.length === 1 && good.counts().ready === 1, "ready then one push");
good.channel.update({ ...push, context_id: "other" });
check(good.port.closed && good.counts().removed === 1, "context changes close port and remove frame");
good.port.receive({ v: 1, op: "ready" });
check(good.port.sent.length === 1, "late message cannot revive retired mount");
const foreign = mount(); foreign.channel.receive({ ...foreign.event, source: {} as Window });
check(!foreign.port.closed && foreign.counts().ready === 0, "unrelated listeners own foreign ports, no data disclosed");
const foreignOrigin = mount(); foreignOrigin.channel.receive({ ...foreignOrigin.event, origin: "https://foreign.example" });
check(foreignOrigin.port.closed && foreignOrigin.counts().failed === 1, "owned frame with foreign origin fails closed");
const wrong = mount(); wrong.channel.receive({ ...wrong.event, data: { ...init, generation: 3 } });
check(wrong.port.closed && wrong.counts().failed === 1, "wrong generation fails closed");
const twice = mount(); twice.channel.receive(twice.event); twice.channel.receive(twice.event);
check(twice.port.closed && twice.counts().failed === 1, "second init closes mount");
const navigate = mount(); navigate.channel.receive(navigate.event); navigate.channel.load(); navigate.channel.load();
check(navigate.port.closed && navigate.counts().removed === 1, "self navigation tears down");
const forged = mount(); forged.channel.receive(forged.event); forged.port.receive({ v: 1, op: "rpc", actor: "operator" });
check(forged.port.closed && forged.counts().failed === 1, "no child RPC authority");

// CAD-1025: publish-intents.v1 is opt-in at `ready`; the first PUSH already
// carries the latest projection, and a forged opt-in closes the mount.
const intents = { status: "ok" as const, withheld: 0, rows: [{ intent_id: "i", install_id: "i", context_id: "c", run_id: "r",
  effect_id: "e", state: "queued" as const, channel: "instagram" as const, destination_id: "d", due_epoch: 1, timezone: "UTC" }] };
const extended: ScreenPush = { ...push, runs: [{ id: "r", state: "running", title: "t", snapshot_digest: "s", workflow: { title: "t" }, context_id: "c" }],
  outbox: [{ effect_id: "e", state: "done", title: "t", run_id: "r", context_id: "c" }],
  publish_intents: { status: "loading", withheld: 0, rows: [] } };
check(parseChild({ v: 1, op: "ready", accepts: ["publish-intents.v1"] }), "intent opt-in accepted");
for (const accepts of [[], ["publish-intents.v2"], ["publish-intents.v1", "publish-intents.v1"], "publish-intents.v1", ["publish-intents.v1", "grants"]])
  check(!parseChild({ v: 1, op: "ready", accepts }), `forged opt-in refused: ${JSON.stringify(accepts)}`);
const opted = mount(); opted.channel.update(extended); opted.channel.receive(opted.event);
opted.channel.update({ ...extended, publish_intents: intents });
check(opted.port.sent.length === 0, "no push before ready even after updates");
opted.port.receive({ v: 1, op: "ready", accepts: ["publish-intents.v1"] });
const first = opted.port.sent[0] as ScreenPush;
check(opted.port.sent.length === 1 && first.publish_intents?.rows[0]?.intent_id === "i", "first push carries the latest intents");
const bare = mount(); bare.channel.update(extended); bare.channel.receive(bare.event); bare.port.receive({ v: 1, op: "ready" });
const legacy = bare.port.sent[0] as Record<string, unknown> & ScreenPush;
check(!("publish_intents" in legacy) && !("context_id" in legacy.runs[0]) && !("run_id" in legacy.outbox[0]),
  "a child that did not opt in receives the exact v1 shape");
bare.channel.update({ ...extended, publish_intents: intents });
check(!("publish_intents" in (bare.port.sent[1] as object)), "later updates keep the negotiated shape");
const forgedOptIn = mount(); forgedOptIn.channel.receive(forgedOptIn.event);
forgedOptIn.port.receive({ v: 1, op: "ready", accepts: ["publish-intents.v1", "credentials"] });
check(forgedOptIn.port.closed && forgedOptIn.port.sent.length === 0, "forged opt-in closes the mount with nothing sent");
const chatOptIn = mount(); chatOptIn.channel.update(extended); chatOptIn.channel.receive(chatOptIn.event); chatOptIn.port.receive({ v: 1, op: "ready", accepts: ["chat-directive.v1"] });
check(chatOptIn.port.closed && chatOptIn.counts().failed === 1 && chatOptIn.port.sent.length === 0, "a workspace screen sending the chat opt-in is closed with nothing pushed");
const unextended = mount(); unextended.channel.receive(unextended.event); unextended.port.receive({ v: 1, op: "ready", accepts: ["publish-intents.v1"] });
check(unextended.port.closed && unextended.port.sent.length === 0, "an opted-in child never gets a push without publish_intents");
const switched = mount(); switched.channel.update(extended); switched.channel.receive(switched.event); switched.port.receive({ v: 1, op: "ready", accepts: ["publish-intents.v1"] });
switched.channel.update({ ...extended, context_id: "other", publish_intents: intents });
check(switched.port.closed && switched.port.sent.length === 1, "intents for a switched context never reach the old mount");
const overCap = { ...extended, publish_intents: { status: "ok" as const, withheld: 0,
  rows: Array.from({ length: 100 }, (_, k) => ({ ...intents.rows[0], intent_id: `i${k}`, destination_id: "d".repeat(1400) })) } };
const heavyOpt = mount(); heavyOpt.channel.update(overCap); heavyOpt.channel.receive(heavyOpt.event);
heavyOpt.port.receive({ v: 1, op: "ready", accepts: ["publish-intents.v1"] });
check(!heavyOpt.port.closed && (heavyOpt.port.sent[0] as ScreenPush).publish_intents?.status === "unavailable",
  "an opted-in push over the cap degrades to unavailable intents");
const heavyBare = mount(); heavyBare.channel.update(overCap); heavyBare.channel.receive(heavyBare.event);
heavyBare.port.receive({ v: 1, op: "ready" });
check(!heavyBare.port.closed && !("publish_intents" in (heavyBare.port.sent[0] as object)), "a legacy child is sized on its own shape");
const badLink = mount(); badLink.channel.update({ ...extended, outbox: [{ ...extended.outbox[0], run_id: "" }] });
badLink.channel.receive(badLink.event); badLink.port.receive({ v: 1, op: "ready", accepts: ["publish-intents.v1"] });
check(badLink.port.closed && badLink.port.sent.length === 0 && badLink.counts().failed === 1 && badLink.counts().ready === 0,
  "out-of-range linkage closes an opted-in mount with nothing sent and no ready");

// CAD-1177: a declared tool alias is advertised in `actions`; an undeclared
// one is not; advertising needs a minted action context and a tool host.
const tokenA = "c".repeat(64);
const toolReceipt = { ...receipt, action_token: tokenA, tools: ["instagram.read"] };
check(parseMount(toolReceipt, "main"), "a mount receipt may name declared tool aliases");
check(!parseMount({ ...toolReceipt, tools: ["Bad Alias"] }, "main"), "a malformed tool alias refuses the receipt");
check(!parseMount({ ...toolReceipt, tools: ["a.b", "a.b"] }, "main"), "duplicate tool aliases refuse the receipt");
check(!parseMount({ ...toolReceipt, extra: 1 }, "main"), "an unknown receipt field still refuses");
const actingPush: ScreenPush = { ...push, defaults: { context_id: "c", revision: 0, values: {} }, readiness: { ok: true, blockers: [] }, publish_intents: { status: "ok", withheld: 0, rows: [] }, actions: ["read.run"] };
const toolHost = { call: async () => ({ ok: true }), planner: () => null, onSlot: () => {}, onLink: () => {},
  invoke: async () => ({ ok: true }) } as unknown as ConstructorParameters<typeof ScreenChannel>[7];
function actingMount(r: typeof toolReceipt | typeof receipt, host: typeof toolHost) {
  const source = {} as Window; const port = new Port();
  const channel = new ScreenChannel(source, r, actingPush, () => {}, () => {}, () => {}, undefined, host);
  channel.receive({ source, origin: "null", data: init, ports: [port as unknown as MessagePort] });
  port.receive({ v: 1, op: "ready", accepts: ["screen.v2", "screen-actions.v1"] });
  return port;
}
const advertised = (actingMount(toolReceipt, toolHost).sent[0] as ScreenPush).actions!;
check(advertised.includes("instagram.read") && !advertised.includes("facebook.read") && advertised.includes("read.run"),
  "a declared alias appears in actions and an undeclared one does not");
check(!(actingMount({ ...toolReceipt, action_token: null } as never, toolHost).sent[0] as ScreenPush).actions!.includes("instagram.read"),
  "no minted action context: nothing is advertised");
check(!(actingMount(toolReceipt, { ...toolHost, invoke: undefined } as never).sent[0] as ScreenPush).actions!.includes("instagram.read"),
  "no tool host: nothing is advertised");

// Gates that must each be the reason nothing is advertised or accepted.
const noActionsPush: ScreenPush = { ...actingPush, actions: [] };
function actingMountWith(r: typeof toolReceipt, pushed: ScreenPush, accepts: string[]) {
  const source = {} as Window; const port = new Port();
  const channel = new ScreenChannel(source, r, pushed, () => {}, () => {}, () => {}, undefined, toolHost);
  channel.receive({ source, origin: "null", data: init, ports: [port as unknown as MessagePort] });
  port.receive({ v: 1, op: "ready", accepts });
  return port;
}
check(!(actingMountWith(toolReceipt, actingPush, ["screen.v2"]).sent[0] as ScreenPush).actions!.includes("instagram.read"),
  "a v2 child that did not opt into screen-actions.v1 is advertised no tool alias");
check((actingMountWith(toolReceipt, noActionsPush, ["screen.v2", "screen-actions.v1"]).sent[0] as ScreenPush).actions!.length === 0,
  "a non-operator projection (empty actions) is never given a tool alias");
check(!parseMount({ ...receipt, action_token: "not-a-token" }, "main"), "any string action_token is not accepted");
check(!parseMount({ ...receipt, action_token: 7 }, "main"), "a non-string action_token is refused");
check(!parseMount({ ...receipt, action_token: "b".repeat(64) }, "main"), "an action token equal to the frame capability is refused");
check(parseMount({ ...receipt, action_token: "d".repeat(64) }, "main"), "a distinct 64-hex action token is accepted");
// A dotted tool alias in a host-held draft image ref parses (a literal dot, not a backslash escape).
check(JSON.stringify(parseDraftImageRef("draft-image:d1:instagram.read")) === JSON.stringify({ draftId: "d1", alias: "instagram.read" }),
  "a dotted-alias draft image ref parses");
check(parseDraftImageRef("draft-image:d1:instagram..read") === null && parseDraftImageRef("draft-image:d1:Bad.Alias") === null &&
  parseDraftImageRef("image:d1") === null, "a malformed draft image ref does not parse");
// SOC-55: only the host-created drawer child reads its own open context;
// a primary (or another mount) is refused, and the value is host-local.
const ctxToken = "e".repeat(64);
const drawerReceipt = { ...receipt, action_token: ctxToken };
const drawerPush = { ...actingPush, v: 2 } as ScreenPush;
const drawerHost = { ...toolHost, onScreenDrawer: () => {} } as unknown as ConstructorParameters<typeof ScreenChannel>[7];
function contextMount(role: "primary" | "drawer", context: unknown) {
  const source = {} as Window; const port = new Port();
  const channel = new ScreenChannel(source, drawerReceipt, drawerPush, () => {}, () => {}, () => {}, undefined, drawerHost);
  channel.surfaceRole = role;
  channel.setDrawerContext(context);
  channel.receive({ source, origin: "null", data: init, ports: [port as unknown as MessagePort] });
  port.receive({ v: 1, op: "ready", accepts: ["screen.v2", "screen-actions.v1", "screen-surfaces.v1"] });
  return { channel, port };
}
const child = contextMount("drawer", { receipt_id: "app-tool-1", post_id: "post-1" });
child.port.receive({ v: 2, op: "call", id: "ctx-1", verb: "screen.drawer.context", args: {} });
const ctxReply = child.port.sent.find(m => (m as { id?: string }).id === "ctx-1") as { ok?: boolean; data?: { context?: unknown } } | undefined;
check(ctxReply?.ok === true && JSON.stringify(ctxReply.data?.context) === JSON.stringify({ receipt_id: "app-tool-1", post_id: "post-1" }),
  "the drawer child reads its own open context");
check(!child.port.closed, "reading the context does not close the drawer");
const primaryCtx = contextMount("primary", { secret: "x" });
primaryCtx.port.receive({ v: 2, op: "call", id: "ctx-2", verb: "screen.drawer.context", args: {} });
check(primaryCtx.port.closed, "only a drawer child may read the host context");
const emptyCtx = contextMount("drawer", null);
emptyCtx.port.receive({ v: 2, op: "call", id: "ctx-3", verb: "screen.drawer.context", args: {} });
const emptyReply = emptyCtx.port.sent.find(m => (m as { id?: string }).id === "ctx-3") as { ok?: boolean; data?: { context?: unknown } } | undefined;
check(emptyReply?.ok === true && emptyReply.data?.context === null, "an open without context reads null");
// A presentation-only peer has no independently mounted child to receive a
// context. Refuse the handoff rather than claiming it was delivered.
const legacyDrawerCalls: unknown[] = [];
const legacyDrawerSource = {} as Window;
const legacyDrawerPort = new Port();
const legacyDrawerHost = { ...toolHost, onScreenDrawer: (...args: unknown[]) => legacyDrawerCalls.push(args) } as unknown as ConstructorParameters<typeof ScreenChannel>[7];
const legacyDrawer = new ScreenChannel(legacyDrawerSource, drawerReceipt, drawerPush, () => {}, () => {}, () => {}, undefined, legacyDrawerHost);
legacyDrawer.receive({ source: legacyDrawerSource, origin: "null", data: init, ports: [legacyDrawerPort as unknown as MessagePort] });
legacyDrawerPort.receive({ v: 1, op: "ready", accepts: ["screen.v2", "screen-actions.v1", "screen-presentation.v1"] });
legacyDrawerPort.receive({ v: 2, op: "call", id: "legacy-ctx", verb: "screen.drawer.open", args: { surface: "post-context-v1", context: { receipt_id: "app-tool-1", post_id: "post-1" } } });
const refusedLegacy = legacyDrawerPort.sent.find(m => (m as { id?: string }).id === "legacy-ctx") as { ok?: boolean } | undefined;
check(refusedLegacy?.ok === false && legacyDrawerCalls.length === 0, "presentation-only peers refuse context handoffs");
legacyDrawerPort.receive({ v: 2, op: "call", id: "legacy-open", verb: "screen.drawer.open", args: { surface: "settings" } });
const openedLegacy = legacyDrawerPort.sent.find(m => (m as { id?: string }).id === "legacy-open") as { ok?: boolean } | undefined;
check(openedLegacy?.ok === true && legacyDrawerCalls.length === 1, "legacy context-free opens still work");

// CAD-2268: generic screen-locale.v1 and {v:2, op:"leave", unsaved} work on
// any screen.v2 mount; legacy CRM tokens and non-v2 mounts are refused.
check(SCREEN_LOCALE_V1 === "screen-locale.v1", "generic locale token exported");
check(!parseChild({ v: 1, op: "ready", accepts: ["screen.v2", "crm-locale.v1"] }), "legacy crm-locale.v1 token is refused");
check(parseChild({ v: 1, op: "ready", accepts: ["screen.v2", "screen-actions.v1", "screen-media.v1", "screen-surfaces.v1", "screen-locale.v1"] }),
  "up to five ready capabilities parse");
check(parseChild({ v: 1, op: "ready", accepts: ["screen.v2", "screen-actions.v1", "screen-records.v1", "screen-surfaces.v1", "screen-locale.v1", "screen-audio.v1"] }),
  "up to six ready capabilities parse");
check(!parseChild({ v: 1, op: "ready", accepts: ["publish-intents.v1", "screen.v2", "screen-actions.v1", "screen-media.v1", "screen-surfaces.v1", "screen-locale.v1", "screen-audio.v1"] }),
  "more than six ready capabilities refused");
check(parseChild({ v: 2, op: "leave", unsaved: true })?.op === "leave", "v2 leave boolean parses");
check(!parseChild({ v: 1, op: "leave", unsaved: true }), "v1 leave refused");
check(!parseChild({ v: 2, op: "leave", unsaved: "yes" }), "non-boolean leave refused");
check(!parseChild({ v: 2, op: "leave", unsaved: true, extra: 1 }), "extra leave field refused");

const unsavedEvents: boolean[] = [];
const genericLocaleSource = {} as Window;
const genericLocalePort = new Port();
const genericLocaleChannel = new ScreenChannel(
  genericLocaleSource,
  toolReceipt,
  { ...actingPush, presentation_locale: "zh-TW" },
  () => {},
  () => {},
  () => {},
  undefined,
  toolHost,
  undefined,
  flag => unsavedEvents.push(flag),
);
genericLocaleChannel.receive({ source: genericLocaleSource, origin: "null", data: init, ports: [genericLocalePort as unknown as MessagePort] });
genericLocalePort.receive({ v: 1, op: "ready", accepts: ["screen.v2", "screen-actions.v1", "screen-locale.v1"] });
check(!genericLocalePort.closed && (genericLocalePort.sent[0] as ScreenPush).presentation_locale === "zh-TW",
  "non-records screen.v2 mount opting into screen-locale.v1 receives presentation_locale");
genericLocalePort.receive({ v: 1, op: "state", data: JSON.stringify({ contract: "crm-leave.v1", unsaved: true }) });
check(unsavedEvents.length === 0, "opaque state never toggles leave guard");
genericLocalePort.receive({ v: 2, op: "leave", unsaved: true });
genericLocalePort.receive({ v: 2, op: "leave", unsaved: false });
check(JSON.stringify(unsavedEvents) === "[true,false]", "v2 leave toggles unsaved warning on any screen.v2 mount");
genericLocalePort.receive({ v: 2, op: "call", id: "forged-locale", verb: "read.run", args: { presentation_locale: "en" } });
check(genericLocalePort.closed, "frame cannot smuggle presentation_locale in call args");

const noLocaleOptIn = actingMountWith(toolReceipt, { ...actingPush, presentation_locale: "zh-TW" }, ["screen.v2", "screen-actions.v1"]);
check(!("presentation_locale" in (noLocaleOptIn.sent[0] as ScreenPush)), "presentation_locale is stripped without screen-locale.v1 opt-in");

const v1LocaleSource = {} as Window;
const v1LocalePort = new Port();
const v1LocaleChannel = new ScreenChannel(v1LocaleSource, receipt, push, () => {}, () => {}, () => {});
v1LocaleChannel.receive({ source: v1LocaleSource, origin: "null", data: init, ports: [v1LocalePort as unknown as MessagePort] });
v1LocalePort.receive({ v: 1, op: "ready", accepts: ["screen-locale.v1"] });
check(v1LocalePort.closed, "screen-locale.v1 without screen.v2 closes the mount");

const v1Leave = mount();
v1Leave.channel.receive(v1Leave.event);
v1Leave.port.receive({ v: 1, op: "ready" });
v1Leave.port.receive({ v: 2, op: "leave", unsaved: true });
check(v1Leave.port.closed, "v1 mount closes on v2 leave op");

// CAD-2272: generic screen-audio.v1 capability and {v:2, op:"audio", id, mode}.
check(SCREEN_AUDIO_V1 === "screen-audio.v1", "generic audio token exported");
check(parseChild({ v: 2, op: "audio", id: "aud-1", mode: "record" })?.op === "audio", "audio record request parses");
check(parseChild({ v: 2, op: "audio", id: "aud-2", mode: "upload" })?.op === "audio", "audio upload request parses");
check(!parseChild({ v: 1, op: "audio", id: "aud-1", mode: "record" }), "v1 audio request refused");
check(!parseChild({ v: 2, op: "audio", id: "aud-1", mode: "stream" }), "unknown audio mode refused");
check(!parseChild({ v: 2, op: "audio", id: "aud-1", mode: "record", audio_base64: "AAAA" }), "forged audio payload field refused");
check(!parseChild({ v: 2, op: "audio", id: "bad id", mode: "record" }), "malformed audio id refused");
check(
  parseScreenAudioCapture({
    transcript: "Action items agreed.",
    duration_seconds: 95,
    held_at: "2026-11-13T03:00:00.000Z",
    source: "recording",
  })?.duration_seconds === 95,
  "valid ScreenAudioCapture parses",
);
check(
  !parseScreenAudioCapture({
    transcript: "Action items agreed.",
    duration_seconds: 95,
    held_at: "2026-11-13T03:00:00.000Z",
    source: "recording",
    action_token: "leaked",
  }),
  "extra ScreenAudioCapture field refused",
);

// Forged incoming audio_capture is stripped unless all gates hold.
const noAudioOptIn = actingMountWith(toolReceipt, { ...actingPush, audio_capture: 1 }, ["screen.v2", "screen-actions.v1"]);
check(!("audio_capture" in (noAudioOptIn.sent[0] as ScreenPush)), "audio_capture stripped without screen-audio.v1 opt-in");
const noAudioHost = actingMountWith(toolReceipt, { ...actingPush, audio_capture: 1 }, ["screen.v2", "screen-actions.v1", "screen-audio.v1"]);
check(!("audio_capture" in (noAudioHost.sent[0] as ScreenPush)), "audio_capture stripped without host captureAudio handler");

// Sending op:"audio" without screen-audio.v1 opt-in closes the mount.
noAudioOptIn.receive({ v: 2, op: "audio", id: "forged-aud", mode: "record" });
check(noAudioOptIn.closed, "audio op without screen-audio.v1 opt-in closes mount");

void (async () => {
  let activeSignals: AbortSignal[] = [];
  let releaseFirst: ((res: { ok: true; data: unknown }) => void) | null = null;
  const audioHost = {
    ...toolHost,
    captureAudio: (_mode: "record" | "upload", opts: { actionToken: string; signal: AbortSignal }) => {
      check(opts.actionToken === tokenA, "captureAudio receives host-held actionToken");
      activeSignals.push(opts.signal);
      return new Promise<{ ok: true; data: unknown }>(resolve => {
        releaseFirst = resolve;
      });
    },
  } as unknown as ConstructorParameters<typeof ScreenChannel>[7];

  const noTokenAudioSource = {} as Window;
  const noTokenAudioPort = new Port();
  const noTokenAudioChannel = new ScreenChannel(
    noTokenAudioSource,
    receipt,
    actingPush,
    () => {},
    () => {},
    () => {},
    undefined,
    audioHost,
  );
  noTokenAudioChannel.receive({ source: noTokenAudioSource, origin: "null", data: init, ports: [noTokenAudioPort as unknown as MessagePort] });
  noTokenAudioPort.receive({ v: 1, op: "ready", accepts: ["screen.v2", "screen-audio.v1"] });
  check(!("audio_capture" in (noTokenAudioPort.sent[0] as ScreenPush)), "audio_capture omitted without receipt action_token");

  const audioSource = {} as Window;
  const audioPort = new Port();
  const audioChannel = new ScreenChannel(
    audioSource,
    toolReceipt,
    actingPush,
    () => {},
    () => {},
    () => {},
    undefined,
    audioHost,
  );
  audioChannel.receive({ source: audioSource, origin: "null", data: init, ports: [audioPort as unknown as MessagePort] });
  audioPort.receive({ v: 1, op: "ready", accepts: ["screen.v2", "screen-audio.v1"] });
  check((audioPort.sent[0] as ScreenPush).audio_capture === 1, "audio_capture projected when opted in with action_token and host");

  audioPort.receive({ v: 2, op: "audio", id: "aud-1", mode: "record" });
  audioPort.receive({ v: 2, op: "audio", id: "aud-2", mode: "upload" });
  const conflictReply = audioPort.sent.find(m => (m as { id?: string }).id === "aud-2") as { ok?: boolean; refusal?: { code?: string } } | undefined;
  check(conflictReply?.ok === false && conflictReply.refusal?.code === "conflict" && !audioPort.closed,
    "concurrent audio capture refuses in-place with conflict");

  releaseFirst!({
    ok: true,
    data: {
      transcript: "Captured transcript",
      duration_seconds: 12,
      held_at: "2026-11-13T03:00:00.000Z",
      source: "recording",
    },
  });
  await Promise.resolve();
  await Promise.resolve();
  const okReply = audioPort.sent.find(m => (m as { id?: string }).id === "aud-1") as { ok?: boolean; data?: { transcript?: string } } | undefined;
  check(okReply?.ok === true && okReply.data?.transcript === "Captured transcript", "audio-reply delivers validated transcript");

  // Channel close aborts an in-flight captureAudio signal.
  activeSignals = [];
  audioPort.receive({ v: 2, op: "audio", id: "aud-3", mode: "upload" });
  check(activeSignals.length === 1 && !activeSignals[0]!.aborted, "in-flight signal starts active");
  audioChannel.close();
  check(activeSignals[0]!.aborted, "closing channel aborts in-flight captureAudio signal");

  console.log("screen lifecycle adversarial checks pass");
})();
