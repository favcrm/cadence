declare function require(name: "node:fs"): { readFileSync(path: string, encoding: "utf8"): string };
import { appHandoffReview } from "../src/lib/appHandoffs";

// Copied byte-for-byte from AgenticOS packages/contracts/fixtures/app-handoff-review.json.
const fixture = JSON.parse(require("node:fs").readFileSync("tests/fixtures/app-handoff-review.json", "utf8"));
const request = (send = false) => appHandoffReview("tc_owner", fixture.review.handoff_id, send, new AbortController().signal);

function check(ok: boolean, message: string): void {
  if (!ok) throw new Error(message);
}

async function verify() {
  const actualFetch = globalThis.fetch;
  try {
    const methods: string[] = [];
    globalThis.fetch = async (url, init) => {
      check(url === `/api/assist/app-handoffs/${fixture.review.handoff_id}/review?conversation=tc_owner`, "private review URL");
      check(new Headers(init?.headers).get("X-Cadence-App-Handoff-Review") === "1", "explicit protocol");
      check(init?.body === undefined, "client must not submit draft arguments");
      methods.push(init?.method ?? "");
      return Response.json(init?.method === "POST"
        ? { review: { ...fixture.review, action_id: "a".repeat(32) } }
        : fixture);
    };
    check((await request())?.totalMinor === 20000, "exact draft values");
    check((await request(true))?.actionId === "a".repeat(32), "exact queue receipt");
    check(methods.join() === "GET,POST", "read versus mutation");

    globalThis.fetch = async () => new Response(null, { status: 404 });
    check(await request() === null, "old host stays preview-only");
    globalThis.fetch = async () => Response.json(fixture);
    check(await request(true).then(() => false, () => true), "missing queue receipt refuses");
    globalThis.fetch = async () => Response.json({ review: { ...fixture.review, handoff_id: "b".repeat(32) } });
    check(await request().then(() => false, () => true), "different handoff refuses");
  } finally {
    globalThis.fetch = actualFetch;
  }
}

void verify();
