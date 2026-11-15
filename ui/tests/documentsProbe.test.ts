/**
 * CAD-2286: the Documents nav item is shown only on a hosted board whose
 * capability probe answered `available`. The probe's store asks the board once
 * per private session when it answered `unavailable` (a 404, a 501
 * `not_implemented` or another capability): `Resource` latches only a thrown
 * 501, and `probe()` never throws for an answer, so the latch is the store's.
 */
import { DOCUMENT_CAPABILITY, type DocumentsProbe } from "../src/features/documents/documentsClient";
import { documentsNavHidden, documentsProbe } from "../src/features/documents/documentsProbe";
import { allowPrivateReads, resetPrivateSession } from "../src/lib/privateSession";

function equal(actual: unknown, expected: unknown, why: string): void {
  if (JSON.stringify(actual) !== JSON.stringify(expected))
    throw new Error(`${why}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
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

let requests: string[] = [];
let answer: () => Response = () => new Response("{}", { status: 200 });
globalThis.fetch = (async (input: unknown) => {
  requests.push(String(input));
  return answer();
}) as typeof fetch;

const reply = (status: number, body: unknown) => () =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

const AVAILABLE = {
  capability: DOCUMENT_CAPABILITY,
  baseline: {
    stages: ["draft", "in_review", "approved", "delivered", "archived"],
    requireApproval: true,
    approver: null,
    archiveAfterDays: 30,
    recipientNote: "",
  },
  types: [],
};

/** A new private session: the store forgets what it saw. */
function newSession(next: () => Response): void {
  resetPrivateSession();
  allowPrivateReads();
  requests = [];
  answer = next;
}

const kind = () => documentsProbe.get().data?.kind ?? null;

// ---- the nav rule ----
const available: DocumentsProbe = { kind: "available", ...AVAILABLE };
equal(documentsNavHidden(true, available), false, "a hosted board whose probe is available shows Documents");
equal(documentsNavHidden(false, available), true, "a native board never shows Documents");
equal(documentsNavHidden(undefined, available), true, "a board that has not answered meta hides it");
equal(documentsNavHidden(true, null), true, "the item stays hidden while the probe loads");
equal(documentsNavHidden(true, { kind: "unavailable", reason: "not_served" }), true, "an unserved lifecycle hides it");
equal(documentsNavHidden(true, { kind: "unavailable", reason: "unsupported_capability" }), true, "an unknown capability hides it");
equal(
  documentsNavHidden(true, { kind: "error", failure: { kind: "failed", status: 500, message: "boom" } }),
  true,
  "a failed probe hides it",
);

async function main(): Promise<void> {
  // A 404 is asked once for the session, however often a screen revalidates.
  newSession(reply(404, { ok: false, error: { code: "not_found", message: "no route" } }));
  await documentsProbe.revalidate();
  equal(kind(), "unavailable", "a 404 is unavailable");
  await documentsProbe.revalidate();
  await documentsProbe.revalidate();
  await documentsProbe.invalidate();
  equal(requests.length, 1, "an unavailable answer is not asked again in the session");

  // A new session asks again.
  newSession(reply(200, AVAILABLE));
  equal(documentsProbe.get().data, null, "a session reset forgets the probe");
  await documentsProbe.revalidate();
  equal(requests, ["/api/document-types"], "a new session probes again");
  equal(kind(), "available", "the new session's answer is read");
  await documentsProbe.revalidate();
  equal(requests.length, 2, "an available answer is re-read, so a flag turned off is noticed");

  // A 501 `not_implemented` latches as a 404 does.
  newSession(reply(501, { ok: false, error: { code: "not_implemented", message: "off" } }));
  await documentsProbe.revalidate();
  await documentsProbe.revalidate();
  equal([kind(), requests.length], ["unavailable", 1], "a 501 not_implemented is asked once");

  // Another capability is unavailable to this client, and latches too.
  newSession(reply(200, { ...AVAILABLE, capability: "cad2286.document-lifecycle.v2" }));
  await documentsProbe.revalidate();
  await documentsProbe.revalidate();
  equal([kind(), requests.length], ["unavailable", 1], "an unsupported capability is asked once");

  // An error is never latched: the next revalidate asks again.
  newSession(reply(500, { ok: false, error: { code: "internal", message: "boom" } }));
  await documentsProbe.revalidate();
  equal(kind(), "error", "a 500 is an error, not unavailable");
  answer = reply(200, AVAILABLE);
  await documentsProbe.revalidate();
  equal([kind(), requests.length], ["available", 2], "an error is retried on the next revalidate");

  console.log("documents probe checks passed");
}

void main().catch((err: unknown) => {
  setTimeout(() => {
    throw err;
  }, 0);
});
