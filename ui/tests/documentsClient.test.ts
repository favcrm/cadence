/**
 * CAD-2286: the board's client for the hosted document lifecycle routes
 * (agenticos-v2 `document-routes.ts`). Every call goes through a fake `fetch`
 * that records the request and answers as the hosted board does, so the
 * probe's classification, the exact request bodies and the strict parsing
 * are pinned to the wire.
 */
import * as documents from "../src/features/documents/documentsClient";
import { DOCUMENT_CAPABILITY, DOCUMENT_REFUSAL_CODES, refusalCode } from "../src/features/documents/documentsClient";

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
function equal(actual: unknown, expected: unknown, why: string): void {
  if (JSON.stringify(actual) !== JSON.stringify(expected))
    throw new Error(`${why}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
}

type Seen = { url: string; method: string; headers: Record<string, string>; body: string | null; cache: string | null };
let seen: Seen[] = [];
let answer: (init?: RequestInit) => Response | Promise<Response> = () => new Response("{}", { status: 200 });

function headersOf(init: RequestInit | undefined): Record<string, string> {
  const out: Record<string, string> = {};
  new Headers(init?.headers).forEach((value, key) => {
    out[key] = value;
  });
  return out;
}

// A plain-Node stand-in for the browser's storage, so `sessionHeaders()` has a key to send.
const stored = new Map<string, string>();
Object.defineProperty(globalThis, "localStorage", {
  configurable: true,
  value: {
    getItem: (key: string) => stored.get(key) ?? null,
    setItem: (key: string, value: string) => void stored.set(key, value),
    removeItem: (key: string) => void stored.delete(key),
  },
});

globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  seen.push({
    url: String(input),
    method: init?.method ?? "GET",
    headers: headersOf(init),
    body: typeof init?.body === "string" ? init.body : null,
    cache: init?.cache ?? null,
  });
  return answer(init);
}) as typeof fetch;

const reply = (status: number, body: unknown) => () =>
  new Response(typeof body === "string" ? body : JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
const refusal = (status: number, code: string, message = `refused: ${code}`) =>
  reply(status, { ok: false, error: { code, message } });

function reset(next: (init?: RequestInit) => Response | Promise<Response>): void {
  seen = [];
  answer = next;
}

const baseline = {
  stages: ["draft", "in_review", "approved", "delivered", "archived"],
  requireApproval: true,
  approver: null,
  archiveAfterDays: 30,
  recipientNote: "",
};
const typeRow = {
  label: "quote",
  requireApproval: true,
  approver: "user-mia",
  recipientNote: "Send to the buyer",
  archiveAfterDays: 14,
  state: "confirmed",
  rev: 3,
};
const typesBody = { capability: "cad2286.document-lifecycle.v1", baseline, types: [typeRow] };

const file = {
  id: "art_01",
  version: 2,
  kind: "document",
  title: "Quote for Harbour Co",
  label: "Quote",
  customer: "Harbour Co",
  bytes: 2048,
  digest: "ab".repeat(32),
  created: "2026-11-14T08:00:00.000Z",
  issue: null,
  shared: false,
};
const state = {
  type: "quote",
  stage: "delivered",
  submittedVersion: 2,
  approvedVersion: 2,
  selfApproved: false,
  deliveredVersion: 2,
  deliveredAt: 1_794_000_000_000,
  archivedAt: null,
  rev: 5,
};

async function probeChecks(): Promise<void> {
  localStorage.setItem("cadence.sessionKey", "test-session-key");
  equal(DOCUMENT_CAPABILITY, "cad2286.document-lifecycle.v1", "the capability this client speaks");

  reset(reply(200, typesBody));
  const available = await documents.probe();
  equal(seen.length, 1, "the probe is one request");
  equal(seen[0]!.url, "/api/document-types", "the probe reads the type list");
  equal(seen[0]!.method, "GET", "the probe is a read");
  equal(seen[0]!.cache, "no-store", "the probe is never served from the HTTP cache");
  assert(available.kind === "available", "200 with the capability is available");
  equal(available.capability, "cad2286.document-lifecycle.v1", "capability kept");
  equal(available.baseline, baseline, "baseline parsed");
  equal(available.types, [typeRow], "type rows parsed");

  // Additive fields are tolerated at every level.
  reset(reply(200, { ...typesBody, extra: 1, baseline: { ...baseline, more: true }, types: [{ ...typeRow, later: "x" }] }));
  const additive = await documents.probe();
  assert(additive.kind === "available", "additive fields do not break the probe");
  equal(additive.types[0]!.label, "quote", "row still read");
  assert(!("later" in additive.types[0]!), "unknown fields are not carried into the typed row");

  const notServed = { kind: "unavailable", reason: "not_served" };
  reset(refusal(404, "not_found", "Not found."));
  equal(await documents.probe(), notServed, "404 (flag off or older board) is unavailable");
  reset(reply(404, "<html>no</html>"));
  equal(await documents.probe(), notServed, "a 404 with any body is unavailable");
  reset(refusal(501, "not_implemented"));
  equal(await documents.probe(), notServed, "501 not_implemented is unavailable");
  reset(reply(501, { code: "not_implemented" }));
  equal(await documents.probe(), notServed, "a top-level not_implemented code is read too");

  // Forward compatibility: a newer contract ships a new capability, and this
  // client hides the feature rather than reading a shape it does not know.
  const unsupported = { kind: "unavailable", reason: "unsupported_capability" };
  reset(reply(200, { ...typesBody, capability: "cad2286.document-lifecycle.v2" }));
  equal(await documents.probe(), unsupported, "another capability is unavailable, not an error");
  reset(reply(200, { capability: "cad2286.document-lifecycle.v2", somethingElse: true }));
  equal(await documents.probe(), unsupported, "and its body is not read as v1");

  for (const [why, next] of [
    ["501 with another code", refusal(501, "internal")],
    ["401", refusal(401, "board_session_required")],
    ["500", refusal(500, "internal")],
    ["503", refusal(503, "capability_unavailable")],
    ["502 without JSON", reply(502, "bad gateway")],
    ["200 with malformed JSON", reply(200, "{not json")],
    ["200 with no capability", reply(200, { baseline, types: [] })],
    ["200 with a numeric capability", reply(200, { ...typesBody, capability: 1 })],
    ["200 with a malformed baseline", reply(200, { ...typesBody, baseline: { ...baseline, requireApproval: "yes" } })],
    ["200 with a malformed type row", reply(200, { ...typesBody, types: [typeRow, { ...typeRow, state: "maybe" }] })],
    ["200 with a type row missing rev", reply(200, { ...typesBody, types: [{ ...typeRow, rev: undefined }] })],
    ["200 with types not a list", reply(200, { ...typesBody, types: {} })],
  ] as const) {
    reset(next);
    const out = await documents.probe();
    equal(out.kind, "error", `${why} is an error, never unavailable`);
  }

  reset(refusal(401, "board_session_required", "Sign in again."));
  const unauth = await documents.probe();
  assert(unauth.kind === "error", "401 is an error");
  equal(unauth.failure, { kind: "refused", status: 401, code: "board_session_required", message: "Sign in again." }, "401 keeps its code");

  reset(() => Promise.reject(new TypeError("network down")));
  const offline = await documents.probe();
  assert(offline.kind === "error", "a network failure is an error");
  equal(offline.failure.kind, "failed", "with no status");
}

async function listChecks(): Promise<void> {
  reset(reply(200, { documents: [{ file, document: state }] }));
  const active = await documents.list(false);
  equal(seen[0]!.url, "/api/documents?archived=0", "active list");
  equal(seen[0]!.method, "GET", "list is a read");
  equal(seen[0]!.cache, "no-store", "list is never served from the HTTP cache");
  assert(active.ok, "a well-formed list is read");
  equal(active.value, [{ file, document: state }], "rows parsed");
  equal(typeof active.value[0]!.document.deliveredAt, "number", "deliveredAt is epoch milliseconds");

  const archivedState = { ...state, stage: "archived", archivedAt: 1_794_500_000_000 };
  reset(reply(200, { documents: [{ file: { ...file, issue: "CAD-1", shared: true, more: 1 }, document: { ...archivedState, more: 2 } }] }));
  const archived = await documents.list(true);
  equal(seen[0]!.url, "/api/documents?archived=1", "archived list");
  assert(archived.ok, "additive fields are tolerated");
  equal(archived.value[0]!.document.archivedAt, 1_794_500_000_000, "archivedAt is epoch milliseconds");
  equal(archived.value[0]!.file.issue, "CAD-1", "an issue link is kept");

  reset(reply(200, { documents: [] }));
  const empty = await documents.list(false);
  assert(empty.ok && empty.value.length === 0, "an empty list is a list");

  for (const [why, row] of [
    ["deliveredAt as a string", { file, document: { ...state, deliveredAt: "2026-11-14T08:00:00Z" } }],
    ["archivedAt as a string", { file, document: { ...state, archivedAt: "yesterday" } }],
    ["an unknown stage", { file, document: { ...state, stage: "published" } }],
    ["a missing type", { file, document: { ...state, type: undefined } }],
    ["selfApproved not boolean", { file, document: { ...state, selfApproved: 0 } }],
    ["a fractional version", { file, document: { ...state, submittedVersion: 1.5 } }],
    ["fractional bytes", { file: { ...file, bytes: 1.5 }, document: state }],
    ["a file without an id", { file: { ...file, id: undefined }, document: state }],
    ["a file with a numeric created", { file: { ...file, created: 1 }, document: state }],
    ["a file with shared missing", { file: { ...file, shared: undefined }, document: state }],
    ["no document", { file }],
    ["a row that is not an object", "row"],
  ] as const) {
    reset(reply(200, { documents: [{ file, document: state }, row] }));
    const out = await documents.list(false);
    assert(!out.ok, `${why} makes the whole list invalid`);
    equal(out.failure.kind, "invalid", `${why}: the answer is not the contract`);
  }
  reset(reply(200, { items: [] }));
  const wrong = await documents.list(false);
  assert(!wrong.ok && wrong.failure.kind === "invalid", "no documents list is invalid");
  reset(refusal(404, "not_found", "Not found."));
  const gone = await documents.list(false);
  assert(!gone.ok, "a 404 list is a failure");
  equal(gone.failure, { kind: "refused", status: 404, code: "not_found", message: "Not found." }, "and keeps the code");
}

function expectWrite(why: string, url: string, body: unknown): void {
  equal(seen.length, 1, `${why}: one request`);
  const request = seen[0]!;
  equal(request.url, url, `${why}: path`);
  equal(request.method, "POST", `${why}: method`);
  equal(request.headers["content-type"], "application/json", `${why}: exact content type`);
  equal(request.headers["x-cadence-board"], "1", `${why}: board header`);
  equal(request.headers["x-cadence-session"], "test-session-key", `${why}: session header`);
  if (body === undefined) return;
  assert(request.body !== null, `${why}: has a body`);
  equal(JSON.parse(request.body), body, `${why}: body`);
  equal(Object.keys(JSON.parse(request.body)).sort(), Object.keys(body as object).sort(), `${why}: exactly these keys`);
}

async function writeChecks(): Promise<void> {
  reset(reply(200, { stage: "approved", version: 2, selfApproved: true }));
  const approved = await documents.approve("art_01", 2);
  expectWrite("approve", "/api/documents/art_01/approve", { version: 2 });
  assert(approved.ok, "approve answers its receipt");
  equal(approved.value, { stage: "approved", version: 2, selfApproved: true }, "approve receipt");

  reset(reply(200, { stage: "approved", version: 2 }));
  const partial = await documents.approve("art_01", 2);
  assert(!partial.ok && partial.failure.kind === "invalid", "a receipt missing selfApproved is invalid");
  reset(reply(200, "not json"));
  const unreadable = await documents.approve("art_01", 2);
  assert(!unreadable.ok && unreadable.failure.kind === "invalid", "an unreadable receipt never reads as success");

  reset(reply(200, { stage: "archived" }));
  const archived = await documents.archive("art_01");
  expectWrite("archive", "/api/documents/art_01/archive", undefined);
  assert(archived.ok, "archive answers");
  equal(archived.value, { stage: "archived" }, "archive receipt");

  reset(reply(200, { stage: "delivered" }));
  const restored = await documents.restore("art_01");
  expectWrite("restore", "/api/documents/art_01/restore", undefined);
  assert(restored.ok, "restore answers");
  equal(restored.value, { stage: "delivered" }, "restore receipt");

  reset(reply(200, { stage: "gone" }));
  const odd = await documents.archive("art_01");
  assert(!odd.ok && odd.failure.kind === "invalid", "an unknown stage in a receipt is invalid");

  const pending = { ...typeRow, approver: null, state: "pending", rev: 1 };
  reset(reply(200, { type: pending }));
  const set = await documents.setType("quote", { requireApproval: true, approver: null, recipientNote: "", archiveAfterDays: 30 });
  expectWrite("setType", "/api/document-types/quote", { requireApproval: true, approver: null, recipientNote: "", archiveAfterDays: 30 });
  assert(set.ok, "setType answers the type");
  equal(set.value, pending, "setType receipt");

  reset(reply(200, { type: typeRow }));
  await documents.setType("Sales quote/EU", { requireApproval: false, approver: "user-mia", recipientNote: "n", archiveAfterDays: 7 });
  expectWrite("setType encoded", "/api/document-types/Sales%20quote%2FEU", { requireApproval: false, approver: "user-mia", recipientNote: "n", archiveAfterDays: 7 });

  reset(reply(200, { type: typeRow }));
  const confirmed = await documents.confirmType("Sales quote/EU", 3);
  expectWrite("confirmType", "/api/document-types/Sales%20quote%2FEU/confirm", { rev: 3 });
  assert(confirmed.ok, "confirm answers the type");
  equal(confirmed.value, typeRow, "confirm receipt");

  reset(reply(200, { type: { ...typeRow, rev: "3" } }));
  const badType = await documents.confirmType("quote", 3);
  assert(!badType.ok && badType.failure.kind === "invalid", "a malformed type receipt is invalid");

  reset(reply(200, { stage: "archived" }));
  await documents.archive("art/01 x");
  equal(seen[0]!.url, "/api/documents/art%2F01%20x/archive", "the document id is percent-encoded");
}

async function refusalChecks(): Promise<void> {
  equal(
    [...DOCUMENT_REFUSAL_CODES].sort(),
    [
      "already_shared",
      "approver_unavailable",
      "capability_unavailable",
      "conflict",
      "document_archived",
      "document_not_approved",
      "internal",
      "invalid_request",
      "not_approver",
      "not_creator",
      "not_found",
      "not_in_review",
      "origin_refused",
      "owner_only",
      "owner_required",
      "self_approval",
      "unlabelled",
    ],
    "every refusal code the routes answer",
  );
  const calls: Array<[string, () => Promise<documents.DocumentsResult<unknown>>]> = [
    ["list", () => documents.list(false)],
    ["approve", () => documents.approve("art_01", 2)],
    ["archive", () => documents.archive("art_01")],
    ["restore", () => documents.restore("art_01")],
    ["setType", () => documents.setType("quote", { requireApproval: true, approver: null, recipientNote: "", archiveAfterDays: 30 })],
    ["confirmType", () => documents.confirmType("quote", 1)],
  ];
  for (const code of DOCUMENT_REFUSAL_CODES) {
    for (const [name, call] of calls) {
      reset(refusal(409, code, `msg ${code}`));
      const out = await call();
      assert(!out.ok, `${name} ${code}: a refusal is a failure`);
      equal(out.failure, { kind: "refused", status: 409, code, message: `msg ${code}` }, `${name} surfaces ${code}`);
      equal(refusalCode(out), code, `${name} refusalCode ${code}`);
    }
  }
  reset(refusal(409, "some_future_code", "later"));
  const future = await documents.approve("art_01", 2);
  assert(!future.ok && future.failure.kind === "refused", "an unknown code is still a refusal");
  equal(future.failure.kind === "refused" && future.failure.code, "some_future_code", "and is kept, not dropped");

  reset(reply(500, "upstream exploded"));
  const opaque = await documents.archive("art_01");
  assert(!opaque.ok, "a non-JSON error is a failure");
  equal(opaque.failure.kind, "failed", "with no code to surface");
  equal(opaque.failure.status, 500, "and its status");
  equal(refusalCode(opaque), null, "an uncoded failure has no refusal code");
  reset(reply(200, { stage: "archived" }));
  equal(refusalCode(await documents.archive("art_01")), null, "a success has no refusal code");
}

/** Resolves to what `run` rejected with; throws if it resolved. */
async function rejection(run: () => Promise<unknown>, why: string): Promise<unknown> {
  let value: unknown;
  try {
    value = await run();
  } catch (error) {
    return error;
  }
  throw new Error(`${why}: resolved ${JSON.stringify(value)} instead of rejecting`);
}

const isAbortError = (error: unknown) => error instanceof Error && error.name === "AbortError";

async function abortChecks(): Promise<void> {
  // An already-aborted signal: no request, and a rejection, never `failed`.
  for (const reason of [undefined, "unmount"]) {
    const controller = new AbortController();
    controller.abort(reason);
    reset(reply(200, typesBody));
    const probed = await rejection(() => documents.probe(controller.signal), `probe pre-aborted (${reason})`);
    assert(isAbortError(probed) || probed === reason, `probe pre-aborted (${reason}) rejects with the abort: ${String(probed)}`);
    reset(reply(200, { documents: [] }));
    const listed = await rejection(() => documents.list(false, controller.signal), `list pre-aborted (${reason})`);
    assert(isAbortError(listed) || listed === reason, `list pre-aborted (${reason}) rejects with the abort: ${String(listed)}`);
  }

  // Aborted mid-request with a custom reason: the fetch rejects with that
  // reason (not an AbortError), and it still rejects rather than resolving.
  const hang = (init?: RequestInit) =>
    new Promise<Response>((_, reject) => {
      const signal = init?.signal;
      assert(signal, "the request carries a signal");
      signal.addEventListener("abort", () => reject(signal.reason), { once: true });
    });
  for (const [name, run] of [
    ["probe", (signal: AbortSignal) => documents.probe(signal)],
    ["list", (signal: AbortSignal) => documents.list(true, signal)],
  ] as const) {
    const controller = new AbortController();
    reset(hang);
    const pending = rejection(() => run(controller.signal), `${name} aborted in flight`);
    await Promise.resolve();
    controller.abort("unmount");
    const error = await pending;
    assert(isAbortError(error) || error === "unmount", `${name} in flight rejects with the abort: ${String(error)}`);
  }
}

async function main(): Promise<void> {
  await probeChecks();
  await listChecks();
  await writeChecks();
  await refusalChecks();
  await abortChecks();
  console.log("documentsClient checks passed");
}

void main().catch((err: unknown) => {
  setTimeout(() => {
    throw err;
  }, 0);
});
