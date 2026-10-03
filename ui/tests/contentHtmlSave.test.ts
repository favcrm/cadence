import { contentClient } from "../src/features/app-shell/contentClient";

export {};
/** CAD-1056: the operator save sends exactly one of blocks/html, plus optional text. */

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
  const scope = { installId: "install-a", contextId: "ctx-b" };
  const sent: Array<Record<string, unknown>> = [];
  const realFetch = (globalThis as any).fetch;
  (globalThis as any).fetch = (_url: string, init: { body?: string }) => {
    sent.push(JSON.parse(init.body ?? "{}"));
    return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve({}), text: () => Promise.resolve("{}") });
  };
  try {
    await contentClient.save(scope, { campaignId: "c1", subject: "S", html: "<p>x</p>", text: "x", expectedRevision: 2 });
    const body = sent[0]!;
    assert(body.html === "<p>x</p>" && body.text === "x" && body.expected_revision === 2, "html save body");
    assert(!("blocks" in body), "html save must not carry blocks");

    await contentClient.save(scope, { campaignId: "c1", subject: "S", blocks: [{ type: "paragraph", text: "x" }] });
    assert(!("html" in sent[1]!) && Array.isArray(sent[1]!.blocks), "blocks save body");

    for (const bad of [
      { campaignId: "c1", subject: "S" },
      { campaignId: "c1", subject: "S", blocks: [], html: "<p>x</p>" },
      { campaignId: "c1", subject: "S", html: "<p>x</p>", actor: "operator" },
      { campaignId: "c1", subject: "S", html: "<p>x</p>", contentDigest: "d" },
    ]) {
      let refused = false;
      try {
        await (contentClient as any).save(scope, bad);
      } catch {
        refused = true;
      }
      assert(refused, `client admitted ${JSON.stringify(bad)}`);
    }
    assert(sent.length === 2, "refused saves must not reach the network");
  } finally {
    (globalThis as any).fetch = realFetch;
  }
}

void main();
