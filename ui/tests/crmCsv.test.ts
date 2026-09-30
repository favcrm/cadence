export {};
/**
 * CAD-865 CSV client wire grammar: the preview/import bodies carry
 * exactly the peer's allowlisted fields, invalid text and envelopes
 * refuse before fetch, and a stale preview token can never ship.
 */
declare function require(name: string): any;

function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
async function rejected(
  action: () => Promise<unknown>,
  check: (error: unknown) => boolean,
  why: string,
) {
  try {
    await action();
  } catch (error) {
    assert(check(error), `${why}: ${error}`);
    return;
  }
  throw new Error(`${why}: expected a refusal`);
}

const SCOPE = { installId: "install-crm", contextId: "ctx-a" };
const TOKEN = "sha256:" + "b".repeat(64);
const CSV =
  "record_id,display_name,email,tags,consent_email\n" +
  "customer-9,Chidi Anagonye,chidi@example.com,newcomer,granted\n";

async function main() {
  const { csvActions, checkCsvText, newImportRequestId } = require(
    "../src/features/app-shell/csvClient",
  ) as typeof import("../src/features/app-shell/csvClient");
  const { ApiError } = require("../src/lib/api") as typeof import("../src/lib/api");

  // Shape checks that never reach the wire.
  await rejected(() => Promise.resolve().then(() => checkCsvText("")), (e) => e instanceof ApiError, "empty CSV refuses");
  await rejected(() => Promise.resolve().then(() => checkCsvText("   \n\n")), (e) => e instanceof ApiError, "whitespace-only CSV refuses");
  await rejected(
    () => Promise.resolve().then(() => checkCsvText("record_id,display_name,evil\nr1,x@example.com\n")),
    (e) => e instanceof ApiError,
    "an unknown column refuses",
  );
  // The byte cap refuses before the wire — a header row padded past
  // the daemon's 256 KiB bound.
  await rejected(
    () => Promise.resolve().then(() => checkCsvText("display_name\n" + "a".repeat(CSV_MAX_BYTES_HINT))),
    (e) => e instanceof ApiError,
    "oversize refuses",
  );
  // A header missing display_name refuses — the daemon demands it.
  await rejected(
    () => Promise.resolve().then(() => checkCsvText("record_id,email\nr1,x@example.com\n")),
    (e) => e instanceof ApiError,
    "a missing display_name column refuses",
  );
  // Too many columns refuse before the wire.
  await rejected(
    () => Promise.resolve().then(() => checkCsvText("display_name," + Array.from({ length: 16 }, (_, i) => `record_id`).join(",") + "\n")),
    (e) => e instanceof ApiError,
    "a 17-column header refuses",
  );
  assert(/^csv-[0-9a-f]{24}$/.test(newImportRequestId()), "the request id is identifier-safe");
  assert(newImportRequestId() !== newImportRequestId(), "request ids are unique");

  // Preview sends exactly {csv_text}; the receipt binds its token.
  const seen: { path: string; body: any }[] = [];
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = new URL(String(input), "http://localhost");
    seen.push({ path: url.pathname, body: init?.body ? JSON.parse(String(init.body)) : null });
    if (url.pathname.endsWith("/records/csv-preview")) {
      return new Response(JSON.stringify({
        preview_token: TOKEN,
        row_count: 1,
        summary: { create: 1, update: 0, skip: 0, needs_revision: 0, error: 0 },
        rows: [
          { row: 1, record_id: "customer-9", decision: "create", expected_revision: null, current_revision: null, profile: { schema: 1, display_name: "Chidi Anagonye", email: "chidi@example.com", tags: ["newcomer"], consent: { email: "granted" } }, errors: [], reason: null },
        ],
      }), { status: 200 });
    }
    if (url.pathname.endsWith("/records/csv-import")) {
      return new Response(JSON.stringify({
        request_id: "csv-1", preview_token: TOKEN, replayed: false,
        summary: { applied: 1, skipped: 0, failed: 0 },
        rows: [{ row: 1, record_id: "customer-9", outcome: "created", reason: null }],
      }), { status: 200 });
    }
    throw new Error(`unexpected ${url.pathname}`);
  }) as typeof fetch;

  const preview = await csvActions.preview(SCOPE, CSV);
  equal(seen.at(-1)?.path, "/api/app-installations/install-crm/contexts/ctx-a/records/csv-preview", "preview rides the reserved route");
  equal(Object.keys(seen.at(-1)?.body ?? {}), ["csv_text"], "preview body is exactly csv_text");
  equal(preview.previewToken, TOKEN, "the bound token parses");
  equal(preview.rows[0].decision, "create", "the plan row parses");

  // Import without decisions sends exactly the three id fields.
  seen.length = 0;
  const receipt = await csvActions.import(SCOPE, CSV, TOKEN, "csv-1");
  equal(seen.at(-1)?.path, "/api/app-installations/install-crm/contexts/ctx-a/records/csv-import", "import rides the reserved route");
  equal(Object.keys(seen.at(-1)?.body ?? {}).sort(), ["csv_text", "preview_token", "request_id"], "import body is exactly the allowlist");
  equal(receipt.summary.applied, 1, "the applied count parses");

  // Decisions serialize; malformed ones refuse client-side.
  await csvActions.import(SCOPE, CSV, TOKEN, "csv-2", [
    { row: 2, action: "update", expectedRevision: 7 },
    { row: 3, action: "skip" },
  ]);
  equal(seen.at(-1)?.body.decisions, [
    { row: 2, action: "update", expected_revision: 7 },
    { row: 3, action: "skip" },
  ], "decisions carry row, action and expected_revision");

  await rejected(
    () => csvActions.import(SCOPE, CSV, "not-a-token", "csv-3"),
    (e) => e instanceof ApiError,
    "a malformed preview token refuses",
  );
  await rejected(
    () => csvActions.import(SCOPE, CSV, TOKEN, "csv-3", []),
    (e) => e instanceof ApiError,
    "an empty decisions list refuses",
  );
  await rejected(
    () => csvActions.import(SCOPE, CSV, TOKEN, "csv-3", [{ row: 0, action: "create" }]),
    (e) => e instanceof ApiError,
    "a non-positive row refuses",
  );
  // An update decision may omit expected_revision — the daemon derives
  // it from the plan row (item.expected_revision.or(row.expected_revision))
  // and refuses only when neither exists. The client refuses only a
  // non-positive explicit revision.
  await csvActions.import(SCOPE, CSV, TOKEN, "csv-3", [{ row: 1, action: "update" }]);
  equal(seen.at(-1)?.body.decisions, [{ row: 1, action: "update" }], "an update without a revision still serializes");
  await rejected(
    () => csvActions.import(SCOPE, CSV, TOKEN, "csv-3", [{ row: 1, action: "update", expectedRevision: 0 }]),
    (e) => e instanceof ApiError,
    "a zero revision refuses",
  );
  await rejected(
    () => csvActions.preview({ installId: "install crm", contextId: "ctx-a" }, CSV),
    (e) => e instanceof ApiError,
    "a malformed scope refuses",
  );

  console.log("crm csv client checks passed");
}

const CSV_MAX_BYTES_HINT = 256 * 1024 + 1;
void main();
