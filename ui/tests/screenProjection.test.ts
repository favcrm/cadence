import { screenProjection, screenTag, settleIntentRead } from "../src/features/workspace-apps/screen/screenProjection";
import type { PublishIntent } from "../src/features/workspace-apps/socialPublish";
import { legacyPush, pushBytes, shapeFor, PUSH_BYTES_MAX } from "../src/features/workspace-apps/screen/screenProtocol";
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
const long = { ...run, snapshot: { ...run.snapshot, workflow: { ...run.snapshot.workflow, title: "x".repeat(2048) } } };
check(screenProjection(installation, "main", "brand", [context], [long], []).runs[0].title.length === 512, "host and app title bounds agree");
refused = false;
try { screenProjection(installation, "main", "brand", [context], Array.from({length: 200}, (_, n) => ({ ...long, id: `run${n}` })), []); } catch { refused = true; }
check(refused, "aggregate message size fails closed before transfer");

// CAD-1025 publish-intents.v1: same-scope verified intents only, bounded, no secrets.
const intent = (over: Partial<PublishIntent>): PublishIntent => ({ intent_id: "i1", install_id: "our", context_id: "brand",
  run_id: "run", effect_id: "effect", state: "queued", channel: "instagram", destination_id: "dest-a", caption_digest: "c".repeat(64),
  image_digest: null, frozen_digest: "f".repeat(64), idempotency_key: "PRIVATE KEY", due_epoch: 1_790_000_000,
  timezone: "Asia/Hong_Kong", grant_id: "PRIVATE GRANT", approval_id: "PRIVATE APPROVAL", writer: "w", reviewer: "r",
  permalink: null, receipt: { secret: "PRIVATE RECEIPT" }, refusal: null, upstream: { token: "PRIVATE UPSTREAM" }, ...over });
const scoped = (ctx: string, intents: PublishIntent[], at = { installId: "our", contextId: ctx }) =>
  screenProjection(installation, "main", ctx, [context], [run, { ...run, id: "run2" }, { ...run, id: "loose", context_id: null }], [effect],
    { ...at, status: "ok", intents });
const history = scoped("brand", [intent({ intent_id: "i1", state: "cancelled" }), intent({ intent_id: "i2" }),
  intent({ intent_id: "i3", destination_id: "dest-b", channel: "facebook" })]);
const block = history.publish_intents!;
check(block.status === "ok" && block.withheld === 0 && block.rows.map(r => r.intent_id).join() === "i1,i2,i3",
  "same-run cancelled history and multiple destinations stay separate rows");
check(block.rows[0].state === "cancelled" && block.rows[2].destination_id === "dest-b", "intent state and destination are verbatim");
check(Object.keys(block.rows[0]).sort().join() === "channel,context_id,destination_id,due_epoch,effect_id,install_id,intent_id,run_id,state,timezone",
  "intent rows are closed");
check(!JSON.stringify(history).includes("PRIVATE"), "grant, approval, key, receipt and upstream never pushed");
check(history.runs.every(r => r.context_id === "brand") && history.outbox[0].run_id === "run" && history.outbox[0].context_id === "brand",
  "runs and effects carry full linkage identity");
const hostile = scoped("brand", [
  intent({ intent_id: "foreign-install", install_id: "other" }),
  intent({ intent_id: "foreign-context", context_id: "foreign" }),
  intent({ intent_id: "dangling", run_id: "missing" }),
  intent({ intent_id: "bad-zone", timezone: "Mars/Olympus" }),
  intent({ intent_id: "bad-epoch", due_epoch: 1.5 }),
  intent({ intent_id: "x".repeat(129) }),
  intent({ intent_id: "dup" }), intent({ intent_id: "dup", run_id: "run2" }),
  intent({ intent_id: "kept" }),
]).publish_intents!;
check(hostile.rows.map(r => r.intent_id).join() === "kept" && hostile.withheld === 8,
  "foreign, dangling, malformed, oversize and duplicated rows are withheld and counted");
const wide = scoped("", [intent({ intent_id: "cross", context_id: null }), intent({ intent_id: "loose", run_id: "loose", context_id: null })]).publish_intents!;
check(wide.rows.map(r => r.intent_id).join() === "loose" && wide.withheld === 1,
  "all-installation scope still joins each intent to its run's exact context");
check(scoped("", [intent({ intent_id: "stale" })], { installId: "our", contextId: "brand" }).publish_intents!.status === "loading",
  "a read tagged for another context is never pushed after a switch");
check(scoped("brand", [intent({})], { installId: "other", contextId: "brand" }).publish_intents!.rows.length === 0,
  "a read tagged for another install is never pushed");
check(screenProjection(installation, "main", "brand", [context], [run], [effect]).publish_intents!.status === "loading",
  "no read yet is loading, not an empty calendar");
const capped = scoped("brand", Array.from({ length: 100 }, (_, n) => intent({ intent_id: `cap${n}` }))).publish_intents!;
check(capped.status === "truncated" && capped.rows.length === 100, "the list cap is reported, never claimed complete");
refused = false;
try { scoped("brand", Array.from({ length: 101 }, (_, n) => intent({ intent_id: `over${n}` }))); } catch { refused = true; }
check(refused, "more rows than the daemon cap fail closed");
const legacy = legacyPush(history);
check(!("publish_intents" in legacy) && legacy.runs.every(r => !("context_id" in r)) && legacy.outbox.every(o => !("run_id" in o) && !("context_id" in o)),
  "a child without publish-intents.v1 receives the exact CAD-1006 shape");

// The byte cap applies to the shape each child receives (review should-fix).
const nearCap = (n: number) => screenProjection(installation, "main", "brand", [context],
  Array.from({ length: n }, (_, k) => ({ ...long, id: `near${k}` })), [],
  { installId: "our", contextId: "brand", status: "ok",
    intents: Array.from({ length: 100 }, (_, k) => intent({ intent_id: `n${k}`.padEnd(100, "n"), run_id: `near${k % n}`, destination_id: "d".repeat(100) })) });
let n = 1;
while (pushBytes(legacyPush(nearCap(n + 1))) < PUSH_BYTES_MAX - 6000) n++;
const heavy = nearCap(n);
check(pushBytes(heavy) > PUSH_BYTES_MAX, "fixture: the extended projection is over the cap");
const toLegacy = shapeFor(heavy, false)!;
check(toLegacy && pushBytes(toLegacy) <= PUSH_BYTES_MAX && !("publish_intents" in toLegacy),
  "a legacy child near the cap still gets its exact v1 push");
const toOptedIn = shapeFor(heavy, true)!;
check(toOptedIn && pushBytes(toOptedIn) <= PUSH_BYTES_MAX && toOptedIn.publish_intents!.status === "unavailable" &&
  toOptedIn.publish_intents!.rows.length === 0, "an opted-in child gets an honest unavailable block, not an oversize push");

// Linkage ids meet the app's bounds or the opted-in shape fails closed.
const linked = scoped("brand", [intent({})]);
check(shapeFor(linked, true) !== null, "in-bound linkage is sent");
for (const broken of [
  { ...linked, outbox: [{ ...linked.outbox[0], run_id: "" }] },
  { ...linked, outbox: [{ ...linked.outbox[0], run_id: "r".repeat(129) }] },
  { ...linked, outbox: [{ ...linked.outbox[0], context_id: "c".repeat(129) }] },
  { ...linked, runs: [{ ...linked.runs[0], context_id: "c".repeat(129) }] },
]) {
  check(shapeFor(broken, true) === null, "out-of-range linkage fails closed for an opted-in child");
  check(shapeFor(broken, false) !== null, "a legacy child never sees those fields and is unaffected");
}

// A failed read keeps the last good read of the same scope only.
const good = settleIntentRead(undefined, "our", "brand", [intent({})]);
check(good.status === "ok" && settleIntentRead(good, "our", "brand", null) === good, "a transient failure keeps the last good read");
check(settleIntentRead(good, "our", "", null).status === "unavailable", "a failure never reuses another context's read");
check(settleIntentRead(good, "other", "brand", null).status === "unavailable", "a failure never reuses another install's read");
check(settleIntentRead(undefined, "our", "brand", null).status === "unavailable", "no earlier read is unavailable");
console.log("screen projection scope and privacy checks pass");
