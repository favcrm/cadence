import { workspaceApps } from "../src/features/workspace-apps/workspaceApps";
import { ApiError } from "../src/lib/api";
import { setSessionKey } from "../src/lib/sessionKey";
import { requestHash, retainedRequest, completeRequest } from "../src/features/workspace-apps/requests";

declare function require(name: string): any;

const storage = new Map<string, string>();
Object.defineProperty(globalThis, "sessionStorage", { configurable: true, value: {
  getItem: (key: string) => storage.get(key) ?? null,
  setItem: (key: string, value: string) => storage.set(key, value),
  removeItem: (key: string) => storage.delete(key),
} });
type Call = { path: string; init: RequestInit };
const calls: Call[] = [];
let reply: unknown = {};
let status = 200;
let fault: Error | null = null;
globalThis.fetch = async (input, init) => {
  calls.push({ path: String(input), init: init ?? {} });
  if (fault) throw fault;
  return new Response(JSON.stringify(reply), { status, headers: { "Content-Type": "application/json" } });
};
function equal(actual: unknown, expected: unknown, why: string) {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) throw new Error(`${why}: ${JSON.stringify(actual)} !== ${JSON.stringify(expected)}`);
}
function assert(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }
const latest = () => calls[calls.length - 1];
async function rejected(action: () => Promise<unknown>, check: (error: unknown) => boolean, why: string) {
  try { await action(); } catch (error) { assert(check(error), why); return; }
  throw new Error(`${why}: unexpectedly succeeded`);
}
async function main() {
  const nodeCrypto = require("crypto");
  Object.defineProperty(globalThis, "crypto", { configurable: true, value: { getRandomValues: nodeCrypto.webcrypto.getRandomValues.bind(nodeCrypto.webcrypto) } });
  for (const input of ["", "abc", "香港 café 🚀", "a".repeat(55), "a".repeat(56), "a".repeat(63), "a".repeat(64), "a".repeat(65), "captions".repeat(1800)])
    equal(requestHash(input), nodeCrypto.createHash("sha256").update(input, "utf8").digest("hex"), "Request retention hash agrees with independent SHA-256 implementation");
  const logicalInput = "install-a:context-a:Source facts kept private";
  const retained = retainedRequest(logicalInput);
  assert(/^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/.test(retained), "Request identity is UUID v4 without secure-context-only UUID API");
  equal(retainedRequest(logicalInput), retained, "Retry preserves logical request identity");
  const changedIntent = retainedRequest(`${logicalInput}:changed`);
  assert(changedIntent !== retained, "Different inputs cannot reuse request authority");
  completeRequest(logicalInput);
  const nextIntent = retainedRequest(logicalInput);
  assert(nextIntent !== retained, "Confirmed completion allows a later deliberate request with identical inputs");
  equal(retainedRequest(logicalInput), nextIntent, "The later deliberate intent is again stable across transport uncertainty");
  equal(retainedRequest(`${logicalInput}:changed`), changedIntent, "Completing one intent does not retire another operation");
  assert([...storage.keys()].every(key => !key.includes("Source facts")) && [...storage.values()].every(value => !value.includes("Source facts")), "Source text never persists in request retention storage");
  Object.defineProperty(globalThis, "sessionStorage", { configurable: true, value: { getItem() { throw new Error("Storage blocked"); }, setItem() { throw new Error("Storage blocked"); } } });
  const fallback = retainedRequest("blocked-storage-unique");
  equal(retainedRequest("blocked-storage-unique"), fallback, "Blocked storage retains request in memory for safe same-screen retry");
  completeRequest("blocked-storage-unique");
  assert(retainedRequest("blocked-storage-unique") !== fallback, "Confirmed completion retires in-memory intent even with blocked storage");
  Object.defineProperty(globalThis, "sessionStorage", { configurable: true, value: {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => storage.set(key, value),
    removeItem: (key: string) => storage.delete(key),
  } });
  setSessionKey("qa-tab-session");
  const controller = new AbortController();
  reply = [{ install_id: "install-a", project_link: null }];
  equal(await workspaceApps.installations(controller.signal), reply, "Workspace catalog is an array, not legacy project Apps payload");
  equal(latest().path, "/api/app-installations", "Canonical catalog path");
  equal(latest().init.credentials, "same-origin", "Requests retain same-origin cookie admission");
  equal(latest().init.cache, "no-store", "Private receipts are not cached across operator sessions");
  equal(new Headers(latest().init.headers).get("X-Cadence-Session"), "qa-tab-session", "Reads carry this tab's second credential");

  // Auth cancellation may compose another signal; verify behavior, not identity.
  const normalFetch = globalThis.fetch;
  try {
    globalThis.fetch = (input, init) => new Promise<Response>((_resolve, reject) => {
      calls.push({ path: String(input), init: init ?? {} });
      const signal = init?.signal;
      assert(signal, "Private catalog requests are abortable");
      const cancelled = () => reject(new DOMException("Caller cancelled request", "AbortError"));
      if (signal.aborted) cancelled();
      else signal.addEventListener("abort", cancelled, { once: true });
    });
    const caller = new AbortController();
    const pending = workspaceApps.installations(caller.signal);
    const forwarded = latest().init.signal;
    assert(forwarded, "Catalog fetch receives its cancellation signal");
    caller.abort();
    assert(forwarded.aborted, "Caller abort propagates to the active fetch");
    await rejected(() => pending, (error) => error instanceof DOMException && error.name === "AbortError", "Caller cancellation rejects the catalog request");
  } finally {
    globalThis.fetch = normalFetch;
  }

  const context = { id: "context-a", revision: 4, digest: "context-digest", state: "active" };
  reply = { contexts: [context] };
  equal(await workspaceApps.contexts("install-a"), [context], "Context envelope is unwrapped without rewriting authority");
  reply = { context };
  equal(await workspaceApps.updateContext("install-a", "context-a", { expected_revision: 4, label: "Fav Limited", input_defaults: { content_prompt: "Write from facts", image_prompt: "Use warm colors" } }), context, "Existing context can save both prompt defaults");
  equal(latest().path, "/api/app-installations/install-a/contexts/context-a/update", "Context edit uses scoped update route");
  equal(JSON.parse(String(latest().init.body)), { expected_revision: 4, label: "Fav Limited", input_defaults: { content_prompt: "Write from facts", image_prompt: "Use warm colors" } }, "Edit includes current revision and only declared settings");
  reply = { bindings: [{ id: "binding-a", revision: 2, digest: "binding-digest" }] };
  await workspaceApps.bindings("install-a", "context-a");
  equal(latest().path, "/api/app-installations/install-a/contexts/context-a/bindings", "Brand binding reads cannot silently use installation-wide defaults");
  reply = { runs: [{ id: "run-a", context_id: "context-a", snapshot_digest: "frozen" }] };
  const runs = await workspaceApps.runs("install-a", "context-a");
  equal(runs, (reply as { runs: unknown[] }).runs, "Run snapshots remain server receipts");
  equal(latest().path, "/api/app-runs?install_id=install-a&context_id=context-a", "Run query carries both stable installation and brand scope");
  await workspaceApps.runs("install-a");
  equal(latest().path, "/api/app-runs?install_id=install-a", "Context-free flow omits the selector");

  reply = { id: "run-a", snapshot_digest: "frozen" };
  const create = { install_id: "install-a", workflow: "social-caption", inputs: { source: "Synthetic facts", writer: "writer-a", reviewer: "reviewer-a" }, owner_pm: "pm-a", request_id: "stable-create" };
  await workspaceApps.createRun(create);
  equal(latest().path, "/api/app-runs", "New run uses workspace lifecycle");
  equal(JSON.parse(String(latest().init.body)), create, "Create forwards the original request ID and omits absent context/project");
  equal(latest().init.method, "POST", "Mutation uses POST");
  equal(new Headers(latest().init.headers).get("X-Cadence-Board"), "1", "Mutations retain board proof marker");
  equal(new Headers(latest().init.headers).get("X-Cadence-Session"), "qa-tab-session", "Mutations retain second credential");
  await workspaceApps.approveRun("run-a", "frozen");
  equal(JSON.parse(String(latest().init.body)), { digest: "frozen" }, "Approval forwards exact frozen digest, no fabricated actor");
  await workspaceApps.dispatchRun("run-a");
  equal(JSON.parse(String(latest().init.body)), {}, "Dispatch supplies no caller-owned assignment or result");

  const effect = { effect_id: "effect-a", digest: "full-release-digest", state: "reconcile", needs_you: true, record: { outcome: { kind: "uncertain", verified: true } } };
  reply = { effect };
  equal(await workspaceApps.stageEffect("run-a", { artifact_id: "artifact-a", slot: "publication", request_id: "stable-stage", title: "Synthetic post" }), effect, "Staging preserves the complete effect receipt");
  equal(JSON.parse(String(latest().init.body)), { artifact_id: "artifact-a", slot: "publication", request_id: "stable-stage", title: "Synthetic post" }, "Stage takes artifact identity, never caller text or file path");
  equal(await workspaceApps.decideEffect("effect-a", { digest: "full-release-digest", decision: "accept" }), effect, "Verified read-back does not overwrite uncertain outcome");
  equal(latest().path, "/api/app-effects/effect-a/decide", "Release is an explicit separate decision");
  await workspaceApps.resolveEffect("effect-a", { digest: "full-release-digest", resolution: "acknowledge" });
  equal(JSON.parse(String(latest().init.body)), { digest: "full-release-digest", resolution: "acknowledge" }, "Historical resolution preserves original receipt digest");

  reply = { item: { effect_id: "effect-a", project: null, scope: "app", authority_digest: "authority", input_digest: "input", provenance: { run_id: "run-a" }, post: "Reviewed text" } };
  equal(await workspaceApps.outbox("effect-a"), reply, "Outbox keeps app scope and durable provenance instead of inventing site ownership");
  equal(latest().path, "/api/outbox?effect_id=effect-a", "Only matching actual Local item is read");


  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  const authEvents = new EventTarget();
  Object.defineProperty(globalThis, "window", { configurable: true, value: authEvents });
  let refusals = 0;
  authEvents.addEventListener("cadence:session-required", () => { refusals += 1; });
  try {
    for (const [code, envelope] of [
      ["operator_session_required", { error: "Local operator session expired", code: "operator_session_required" }],
      ["board_session_required", { error: { message: "Hosted board session expired", code: "board_session_required" } }],
    ] as const) {
      status = 401; reply = envelope;
      const message = typeof envelope.error === "string" ? envelope.error : envelope.error.message;
      const beforeRefusal = refusals;
      await rejected(() => workspaceApps.detail("install-a"), error =>
        error instanceof ApiError && error.status === 401 && error.code === code && error.message === message,
      "Both auth refusal envelopes preserve their readable message and code");
      equal(refusals, beforeRefusal + 1, "A confirmed private read refusal immediately notifies the session gate");
    }
    status = 403; reply = { error: { code: "board_session_required", message: "Role cannot read this app" } };
    const beforePermission = refusals;
    await rejected(() => workspaceApps.detail("install-a"), error =>
      error instanceof ApiError && error.status === 403 && error.message === "Role cannot read this app",
    "A role refusal remains a readable permission error");
    equal(refusals, beforePermission, "A normal role 403 does not log out the operator");
    status = 401; reply = { code: "other_auth_failure", error: "Not a board-session refusal" };
    await rejected(() => workspaceApps.detail("install-a"), error => error instanceof ApiError && error.status === 401,
      "An unrelated 401 remains an API error");
    equal(refusals, beforePermission, "An unrecognized 401 does not confirm board session loss");
  } finally {
    if (originalWindow) Object.defineProperty(globalThis, "window", originalWindow);
    else Reflect.deleteProperty(globalThis, "window");
  }
  let before = calls.length;
  status = 409; reply = { error: "authority revision changed" };
  await rejected(() => workspaceApps.decideEffect("effect-a", { digest: "stale", decision: "accept" }), e => e instanceof ApiError && e.status === 409 && e.message === "authority revision changed", "Stale authority is a typed refusal");
  equal(calls.length, before + 1, "A release refusal never retries or refreshes authority and accepts automatically");
  status = 200; reply = null;
  await rejected(() => workspaceApps.detail("install-a"), e => e instanceof ApiError && e.status === 502, "Null receipt is rejected");
  fault = new Error("Connection lost after send"); before = calls.length;
  await rejected(() => workspaceApps.stageEffect("run-a", { artifact_id: "artifact-a", slot: "publication", request_id: "stable-stage", title: "Synthetic post" }), e => e === fault, "Transport uncertainty is preserved");
  equal(calls.length, before + 1, "Uncertain mutations are not retried behind the operator's back");
  fault = null; reply = [];
  setSessionKey("replacement-session");
  await workspaceApps.installations();
  equal(new Headers(latest().init.headers).get("X-Cadence-Session"), "replacement-session", "Session is looked up per call, never captured by the adapter");
  setSessionKey(null);
  await workspaceApps.installations();
  equal(new Headers(latest().init.headers).has("X-Cadence-Session"), false, "Revoked tab credential is never replayed");
  console.log("workspace Apps wire contract checks passed");
}
void main();
