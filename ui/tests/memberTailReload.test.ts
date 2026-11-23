export {};
/**
 * AOS-711 (CAD-2705): a share request card is rewritten in place when it is
 * decided (and a file card is added after it, which renumbers every later
 * `csrid_`), which the member chat's forward-only poll never reads. `tailReload`
 * makes the next read replace what is held with the newest page: nothing is
 * duplicated by the renumbering, and the forward cursor moves on.
 */
declare function require(name: string): any;
declare const process: { cwd(): string; exit(code?: number): never };
const equal = (actual: unknown, expected: unknown, what: string) => {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) throw new Error(`${what}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
};
const fixture = (name: string): any => JSON.parse(require("fs").readFileSync(`${process.cwd()}/tests/fixtures/share-request/${name}.json`, "utf8"));

const conversation = { id: "tc_00000000-0000-4000-8000-000000000001", alias: "master", created: "2026-01-01T00:00:00.000Z", updated: "2026-01-01T00:00:00.000Z" };
const pendingCard = { ...fixture("card-pending"), id: "csrid_2" };
const sharedCard = { ...fixture("card-shared"), id: "csrid_2" };
const before = { role: "operator", kind: "message", text: "Show me Q3's P&L", message: "m-1", id: "csrid_1" };
const reply = (id: string) => ({ role: "assistant", kind: "message", text: "Chris shared it.", id });
const file = { ...fixture("artifact-card-after-shared"), id: "csrid_3" };
const asked: Record<string, unknown>[] = [];
const page = (entries: unknown[], cursor: string, earlier = "csc1_1") => ({ conversation, entries, cursor, before_cursor: earlier, more_before: true, more_after: false });
globalThis.fetch = (async (_input: unknown, init?: RequestInit) => {
  const body = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>;
  asked.push(body);
  const decided = asked.length > 2;
  const answer = body.tail === true
    ? page(decided ? [before, sharedCard, file, reply("csrid_4")] : [before, pendingCard, reply("csrid_3")], decided ? "csc1_4" : "csc1_3")
    : page([], "csc1_3", "csc1_3");
  return new Response(JSON.stringify(answer), { status: 200, headers: { "Content-Type": "application/json" } });
}) as typeof fetch;

async function main() {
  const { memberThreadStore, memberWrite } = require("../src/features/home/memberThread") as typeof import("../src/features/home/memberThread");
  const store = memberThreadStore("scope-tail");
  await store.refresh();
  equal(store.get().data!.entries.map((e) => e.kind), ["message", "share_request", "message"], "the first read is the tail");
  equal((store.get().data!.entries[1]!.payload as any).status, "pending", "pending");

  // An ordinary refresh reads forward only: it cannot see a rewritten row.
  await store.refresh();
  equal(asked[1], { after: "csc1_3", limit: 200 }, "a plain refresh is forward-only");

  memberWrite(store, (cur) => ({ ...cur!, tailReload: true }));
  await store.refresh();
  equal(asked[2], { tail: true, limit: 200 }, "a flagged refresh takes the newest page again");
  const data = store.get().data!;
  equal(data.entries.map((e) => `${e.kind}:${(e.payload as any)?.status ?? ""}`), ["message:", "share_request:shared", "artifact:", "message:"], "the card is shared, its file follows, and no later row is shown twice");
  equal(data.tailReload, undefined, "the flag is spent");
  equal(data.memberPosition, { forward: "csc1_4", before: "csc1_1" }, "the forward cursor moves on");
  await store.refresh();
  equal(asked[3], { after: "csc1_4", limit: 200 }, "and the next read is forward again");
  console.log("memberTailReload ok");
}
main().then(() => process.exit(0), (error) => { console.error(error); process.exit(1); });
