export {};
/**
 * AOS-711 (CAD-2705): a share request card is rewritten in place when it is
 * decided (and a file card is added), which the member chat's forward-only poll
 * never reads. `tailReload` makes the next read take the newest page again:
 * the same row (by id) is replaced where it is, the new one follows, and the
 * older-history position is kept.
 */
declare function require(name: string): any;
declare const process: { cwd(): string; exit(code?: number): never };
const equal = (actual: unknown, expected: unknown, what: string) => {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) throw new Error(`${what}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
};
const fixture = (name: string): any => JSON.parse(require("fs").readFileSync(`${process.cwd()}/tests/fixtures/share-request/${name}.json`, "utf8"));

const conversation = { id: "tc_00000000-0000-4000-8000-000000000001", alias: "master", created: "2026-01-01T00:00:00.000Z", updated: "2026-01-01T00:00:00.000Z" };
const pendingCard = { ...fixture("card-pending"), id: "csrid_card" };
const sharedCard = { ...fixture("card-shared"), id: "csrid_card" };
const file = { ...fixture("artifact-card-after-shared"), id: "csrid_file" };
const asked: Record<string, unknown>[] = [];
const page = (entries: unknown[], cursor: string, before = "csc1_1") => ({ conversation, entries, cursor, before_cursor: before, more_before: true, more_after: false });
globalThis.fetch = (async (_input: unknown, init?: RequestInit) => {
  const body = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>;
  asked.push(body);
  const decided = asked.length > 2;
  const reply = body.tail === true
    ? page(decided ? [sharedCard, file] : [pendingCard], decided ? "csc1_9" : "csc1_5")
    : page([], "csc1_5", "csc1_5");
  return new Response(JSON.stringify(reply), { status: 200, headers: { "Content-Type": "application/json" } });
}) as typeof fetch;

async function main() {
  const { memberThreadStore, memberWrite } = require("../src/features/home/memberThread") as typeof import("../src/features/home/memberThread");
  const store = memberThreadStore("scope-tail");
  await store.refresh();
  equal(store.get().data!.entries.map((e) => e.kind), ["share_request"], "the first read is the tail");
  equal((store.get().data!.entries[0]!.payload as any).status, "pending", "pending");

  // An ordinary refresh reads forward only: it cannot see a rewritten row.
  await store.refresh();
  equal(asked[1], { after: "csc1_5", limit: 200 }, "a plain refresh is forward-only");

  memberWrite(store, (cur) => ({ ...cur!, tailReload: true }));
  await store.refresh();
  equal(asked[2], { tail: true, limit: 200 }, "a flagged refresh takes the newest page again");
  const data = store.get().data!;
  equal(data.entries.map((e) => `${e.kind}:${(e.payload as any).status ?? ""}`), ["share_request:shared", "artifact:"], "the card is replaced in place and its file follows");
  equal(data.tailReload, undefined, "the flag is spent");
  equal(data.memberPosition, { forward: "csc1_9", before: "csc1_1" }, "the older-history position is kept, the forward cursor moves");
  await store.refresh();
  equal(asked[3], { after: "csc1_9", limit: 200 }, "and the next read is forward again");
  console.log("memberTailReload ok");
}
main().then(() => process.exit(0), (error) => { console.error(error); process.exit(1); });
