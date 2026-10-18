export {};
/**
 * CAD-786 send clients: the flat `/api/crm-smtp/*` and
 * `/api/crm-send/*` clients carry exactly each verb's allowlisted
 * body (no forged field ever serializes), terminal states stop the
 * poll, and every receipt labels SMTP acceptance as acceptance —
 * never delivery. The campaign send UI that mounted them moved to the
 * CRM app's own screen (CAD-1519).
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

const SCOPE = { installId: "install-a", contextId: "ctx-b" };

async function main() {
  await wireBodies();
  receiptHelpers();
}

/** Every write serializes exactly its allowlisted fields — and a
 *  smuggled key refuses client-side before fetch is ever reached. */
async function wireBodies() {
  const { sendClient, sendPaths } = require("../src/features/app-shell/sendClient") as typeof import(
    "../src/features/app-shell/sendClient"
  );
  const { ApiError } = require("../src/lib/api") as typeof import("../src/lib/api");

  const seen: { path: string; method: string; body: any }[] = [];
  const realFetch = (globalThis as any).fetch;
  (globalThis as any).fetch = async (input: unknown, init?: RequestInit) => {
    const url = new URL(String(input), "http://localhost");
    seen.push({
      path: `${url.pathname}${url.search}`,
      method: init?.method ?? "GET",
      body: init?.body ? JSON.parse(String(init.body)) : null,
    });
    if (url.pathname === sendPaths.smtpShow) {
      return new Response(JSON.stringify({ binding: {
        install_id: "install-a", context_id: "ctx-b", connection_id: "conn-1",
        auth_revision: 1, link_revision: 3, state: "live", digest: "sha256:link",
        sender: { name: "N", address: "n@example.com" },
        transport: { host: "smtp.example.com", port: 465, tls_mode: "implicit", username: "u" },
      } }), { status: 200, headers: { "Content-Type": "application/json" } });
    }
    return new Response(JSON.stringify({ ok: true }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  };
  try {
    await sendClient.smtpShow(SCOPE);
    equal(seen.at(-1)?.body, { install_id: "install-a", context_id: "ctx-b" }, "smtp show body");

    await sendClient.smtpBind(SCOPE, "conn-1", "req-1");
    equal(
      seen.at(-1)?.body,
      { install_id: "install-a", context_id: "ctx-b", connection_id: "conn-1", request_id: "req-1" },
      "bind body is exactly the four fields",
    );

    await sendClient.smtpRebind(SCOPE, "conn-1", 3);
    equal(
      seen.at(-1)?.body,
      { install_id: "install-a", context_id: "ctx-b", connection_id: "conn-1", expected_revision: 3 },
      "rebind body carries the observed link revision",
    );

    await sendClient.smtpRevoke(SCOPE, 3);
    equal(
      seen.at(-1)?.body,
      { install_id: "install-a", context_id: "ctx-b", expected_revision: 3 },
      "revoke body is exactly three fields",
    );

    await sendClient.smtpTestSend(SCOPE, "launch-1", "operator@example.com");
    equal(
      seen.at(-1)?.body,
      {
        install_id: "install-a",
        context_id: "ctx-b",
        campaign_id: "launch-1",
        to_email: "operator@example.com",
      },
      "test send carries one recipient only — no bulk field exists",
    );

    await sendClient.sendPrepare(SCOPE, "launch-1", "freeze-1", "req-9");
    equal(
      seen.at(-1)?.body,
      {
        install_id: "install-a",
        context_id: "ctx-b",
        campaign_id: "launch-1",
        audience_freeze_id: "freeze-1",
        request_id: "req-9",
      },
      "prepare body is exactly the five fields",
    );

    await sendClient.sendApprove(SCOPE, "send-1", "sha256:digest");
    equal(
      seen.at(-1)?.body,
      { install_id: "install-a", context_id: "ctx-b", send_id: "send-1", send_digest: "sha256:digest" },
      "approve body is exactly the digest-bearing four fields",
    );

    await sendClient.sendResolve(SCOPE, "send-1", "cust-1", "accepted");
    equal(
      seen.at(-1)?.body,
      {
        install_id: "install-a",
        context_id: "ctx-b",
        send_id: "send-1",
        customer_id: "cust-1",
        resolution: "accepted",
      },
      "resolve body is exactly the five fields",
    );

    await sendClient.sendOriginSet("https://cadence.example.com");
    equal(
      seen.at(-1)?.body,
      { unsubscribe_origin: "https://cadence.example.com" },
      "origin set carries the one field",
    );
    await sendClient.sendOriginSet(null);
    equal(seen.at(-1)?.body, { unsubscribe_origin: null }, "origin clear carries null");

    await sendClient.sendShow(SCOPE, "send-1");
    equal(
      seen.at(-1)?.path,
      "/api/crm-send/show?install_id=install-a&context_id=ctx-b&send_id=send-1",
      "show is a GET with the three query ids",
    );
    await sendClient.sendList(SCOPE, "launch-1");
    equal(
      seen.at(-1)?.path,
      "/api/crm-send/list?install_id=install-a&context_id=ctx-b&campaign_id=launch-1",
      "list carries the campaign filter",
    );
    await sendClient.sendList(SCOPE);
    equal(
      seen.at(-1)?.path,
      "/api/crm-send/list?install_id=install-a&context_id=ctx-b",
      "list omits a missing campaign filter",
    );

    // A forged body field refuses before fetch — the harness above
    // counts calls, so `seen` length must not move.
    const before = seen.length;
    const forged: [() => Promise<unknown>, string][] = [
      [() => sendClient.sendApprove(SCOPE, "send-1", "sha256:d"), "baseline ok — counted"],
      [() => (sendClient as any).sendResolve(SCOPE, "send-1", "cust-1", "resend"), "bad resolution"],
      [() => (sendClient as any).smtpRevoke(SCOPE, 0), "zero revision"],
      [() => (sendClient as any).smtpTestSend(SCOPE, "launch-1", "a b@example.com"), "bad address"],
      [() => sendClient.sendOriginSet("ftp://evil.example"), "bad origin scheme"],
      [() => sendClient.sendOriginSet("https://example.com/path"), "origin with path"],
      [() => sendClient.sendOriginSet("http://example.com"), "non-loopback http origin"],
    ];
    for (const [call, why] of forged) {
      try {
        await call();
      } catch (error) {
        assert(error instanceof ApiError, `${why} refuses typed`);
        continue;
      }
      if (why.startsWith("baseline")) continue;
      throw new Error(`${why} passed the client gate`);
    }
    assert(
      seen.length === before + 1,
      "only the baseline call reached the wire — every forged body refused client-side",
    );
  } finally {
    (globalThis as any).fetch = realFetch;
  }
}

/** Pure receipt helpers: terminal states and the acceptance-only claim. */
function receiptHelpers() {
  const { sendTerminal, DELIVERY_CLAIM } = require("../src/features/app-shell/sendClient") as typeof import(
    "../src/features/app-shell/sendClient"
  );
  assert(sendTerminal("completed") && sendTerminal("closed"), "terminal states stop the poll");
  assert(!sendTerminal("sending") && !sendTerminal("prepared"), "live states keep polling");
  equal(DELIVERY_CLAIM, "smtp-acceptance-only", "the claim label is acceptance-only");
  console.log("crm send checks passed");
}

void main();
