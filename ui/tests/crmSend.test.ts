export {};
/**
 * CAD-786 UI send controls: the flat `/api/crm-smtp/*` and
 * `/api/crm-send/*` clients carry exactly each verb's allowlisted
 * body (no forged field ever serializes), the typed-count confirm
 * gates approve, prepare's prerequisites gate before the wire, the
 * progress poll stops on a terminal state and on unmount, only
 * `uncertain` rows offer resolve, and every receipt labels SMTP
 * acceptance as acceptance — never delivery.
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
  await mountedFlow();
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

async function mountedFlow() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/app-installations/install-crm?ctx=ctx-a" });
  for (const name of [
    "window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement",
    "HTMLTextAreaElement", "HTMLSelectElement", "HTMLIFrameElement", "SVGElement",
    "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent",
    "KeyboardEvent", "location", "history", "sessionStorage",
  ])
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  for (const name of ["addEventListener", "removeEventListener"])
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "crypto", {
    value: require("crypto").webcrypto,
    configurable: true,
  });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  Object.defineProperty(globalThis, "EventSource", {
    configurable: true,
    value: class {
      onopen: null = null;
      onerror: null = null;
      constructor() {}
      addEventListener() {}
      removeEventListener() {}
      close() {}
    },
  });
  const loader = require("module"), originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, arguments);
  };

  const React = require("react") as typeof import("react");
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const AppShell = (require("../src/features/app-shell/AppShell") as typeof import(
    "../src/features/app-shell/AppShell"
  )).default;
  const { sendTerminal, DELIVERY_CLAIM } = require("../src/features/app-shell/sendClient") as typeof import(
    "../src/features/app-shell/sendClient"
  );

  assert(sendTerminal("completed") && sendTerminal("closed"), "terminal states stop the poll");
  assert(!sendTerminal("sending") && !sendTerminal("prepared"), "live states keep polling");
  equal(DELIVERY_CLAIM, "smtp-acceptance-only", "the claim label is acceptance-only");

  const install = {
    install_id: "install-crm", title: "CRM", name: "crm", version: "0.1.0",
    digest: "install-digest", catalog_generation: "gen-1", approved: true,
    storage_kind: "workspace", files: [], capabilities: null, connection_slots: [],
  };
  const contents: Record<string, any> = {
    "launch-1": {
      campaign_id: "launch-1", install_id: "install-crm", context_id: "ctx-a", revision: 1,
      subject: "Hello", preheader: "", blocks: [{ type: "paragraph", text: "Hi there" }],
      content_digest: "content-digest-1",
      approval: { revision: 1, digest: "content-digest-1", valid: true, scope: "content-only" },
    },
  };
  const bindings: { current: any } = {
    current: {
      install_id: "install-crm", context_id: "ctx-a", connection_id: "conn-1",
      auth_revision: 1, link_revision: 1, state: "live", digest: "sha256:link1",
      sender: { name: "CRM News", address: "news@example.com" },
      transport: { host: "localhost", port: 465, tls_mode: "implicit", username: "u" },
    },
  };

  const sends: Record<string, any> = {};
  const prepared: Record<string, any> = {};
  let testAccepted = false;
  let showPolls = 0;
  const wireBodies: { path: string; body: any }[] = [];
  const json = (value: unknown, status = 200) =>
    new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
  const refused = (message: string, status = 409) => json({ error: message }, status);
  let smtpError: string | null = null;
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const path = String(input);
    const url = new URL(path, "http://localhost");
    const method = init?.method ?? "GET";
    if (method === "POST" && init?.body) {
      wireBodies.push({ path: url.pathname, body: JSON.parse(String(init.body)) });
    }
    if (method === "POST" && url.pathname === "/api/crm-smtp/show") {
      if (bindings.current === null) {
        return refused("CRM SMTP sender refused or unavailable", 409);
      }
      return json({ binding: bindings.current });
    }
    if (method === "POST" && url.pathname === "/api/crm-smtp/revoke") {
      const body = wireBodies.at(-1)!.body;
      if (bindings.current === null || body.expected_revision !== bindings.current.link_revision) {
        return refused("CRM SMTP sender refused or unavailable", 409);
      }
      bindings.current = null;
      return json({ revoked: true });
    }
    if (method === "POST" && url.pathname === "/api/crm-smtp/test-send") {
      const body = wireBodies.at(-1)!.body;
      const doc = contents[body.campaign_id];
      if (bindings.current === null) return refused("CRM SMTP sender refused or unavailable", 409);
      if (!doc) return refused("CRM SMTP sender refused or unavailable", 409);
      testAccepted = true;
      return json({ receipt: {
        test_send: true, kind: "test", campaign_id: body.campaign_id, to_email: body.to_email,
        accepted: true, smtp_code: 250, smtp_message: "accepted",
        content_revision: doc.revision, content_digest: doc.content_digest,
        link_digest: "sha256:link1", delivery_claim: "smtp-acceptance-only",
      } });
    }
    if (method === "POST" && url.pathname === "/api/crm-send/prepare") {
      const body = wireBodies.at(-1)!.body;
      const doc = contents[body.campaign_id];
      if (!doc || !doc.approval.valid) return refused("email content is not approved at its current revision");
      if (body.audience_freeze_id !== "launch-1-freeze-1" && body.audience_freeze_id !== "freeze-1") return refused("audience freeze is unavailable");
      if (bindings.current === null) return refused("no SMTP sender is bound to this installation and context");
      if (!testAccepted) return refused("an SMTP-accepted test send of this exact content and binding is required first");
      const sendId = `send-${body.request_id}`;
      prepared[sendId] = {
        send_id: sendId, install_id: "install-a", context_id: "ctx-a", campaign_id: body.campaign_id,
        request_id: body.request_id, content_revision: doc.revision, content_digest: doc.content_digest,
        audience_freeze_id: body.audience_freeze_id, audience_digest: "aud-digest-1",
        connection_id: "conn-1", auth_revision: 1, link_revision: 1, link_digest: "sha256:link1",
        max_recipients: 50, unsubscribe_origin: "http://127.0.0.1:3111",
        send_digest: "sha256:senddigest-" + sendId, state: "prepared", close_reason: null,
        created: 1, approved_at: null,
      };
      return json({
        send: prepared[sendId],
        counts: { included: 5, excluded: 1, suppressed_now: 1, final: 3, max_recipients: 50 },
        sample: [{ customer_id: "customer-a", email: "a***@example.com" }],
        send_digest: prepared[sendId].send_digest,
      });
    }
    if (method === "POST" && url.pathname === "/api/crm-send/approve") {
      const body = wireBodies.at(-1)!.body;
      const send = prepared[body.send_id];
      if (!send) return refused("campaign send is unavailable for this installation and context");
      if (send.send_digest !== body.send_digest) return refused("send digest does not match the prepared send");
      send.state = "sending";
      send.approved_at = 2;
      sends[send.send_id] = {
        ...send,
        counts: { queued: 0, submitting: 0, accepted: 3, failed: 0, uncertain: 0, suppressed: 0, closed: 0 },
        deliveries: [
          { send_id: send.send_id, customer_id: "customer-a", email: "a***@example.com", idempotency_key: "k1", state: "accepted", attempts: 1, smtp_code: 250, reason: null, resolved_by: null, delivery_claim: "smtp-acceptance-only" },
          { send_id: send.send_id, customer_id: "customer-c", email: "c***@example.com", idempotency_key: "k2", state: "accepted", attempts: 1, smtp_code: 250, reason: null, resolved_by: null, delivery_claim: "smtp-acceptance-only" },
          { send_id: send.send_id, customer_id: "customer-e", email: "e***@example.com", idempotency_key: "k3", state: "uncertain", attempts: 1, smtp_code: null, reason: "connection lost mid-submission", resolved_by: null, delivery_claim: "smtp-acceptance-only" },
        ],
      };
      return json({
        send: sends[send.send_id], counts: sends[send.send_id].counts,
        deliveries: sends[send.send_id].deliveries, delivery_claim: "smtp-acceptance-only",
      });
    }
    if (method === "POST" && url.pathname === "/api/crm-send/resolve") {
      const body = wireBodies.at(-1)!.body;
      const send = sends[body.send_id];
      const row = send?.deliveries.find((d: any) => d.customer_id === body.customer_id);
      if (!row || row.state !== "uncertain") return refused("only an uncertain delivery can be resolved");
      row.state = body.resolution;
      row.resolved_by = "operator";
      send.counts.uncertain -= 1;
      send.counts[body.resolution] += 1;
      return json({ delivery: { ...row, delivery_claim: "smtp-acceptance-only" } });
    }
    if (url.pathname === "/api/crm-send/show") {
      showPolls += 1;
      const send = sends[url.searchParams.get("send_id") ?? ""];
      if (!send) return refused("campaign send is unavailable for this installation and context", 404);
      // First polls answer `sending`; the third answers `completed` —
      // the poll must stop there.
      if (showPolls >= 3) send.state = "completed";
      return json({ send, counts: send.counts, deliveries: send.deliveries, delivery_claim: "smtp-acceptance-only" });
    }
    if (url.pathname === "/api/crm-send/list") {
      const campaign = url.searchParams.get("campaign_id");
      const rows = Object.values(sends)
        .filter((row) => campaign === null || row.campaign_id === campaign)
        .map((row) => ({
          send_id: row.send_id, campaign_id: row.campaign_id, state: row.state,
          send_digest: row.send_digest, created: row.created, approved_at: row.approved_at,
          counts: row.counts, delivery_claim: "smtp-acceptance-only",
        }));
      return json({ sends: rows });
    }
    if (url.pathname === "/api/crm-send/origin") {
      if (method === "POST") {
        const body = wireBodies.at(-1)!.body;
        if (body.unsubscribe_origin === null) {
          return json({ unsubscribe_origin: null });
        }
        return json({ unsubscribe_origin: body.unsubscribe_origin });
      }
      return json({ unsubscribe_origin: "http://127.0.0.1:3111", stored: false });
    }
    if (path === "/api/connections") return json({ connections: [{
      id: "conn-1", provider: "smtp", account: "isolated", kind: "enrolled",
      revision: 1, registration_digest: "sha256:reg1", scopes: ["email:send"],
      smtp_sender: true, smtp_error: smtpError,
      smtp: smtpError ? null : { host: "localhost", port: 465, tls_mode: "implicit", username: "u", sender: "news@example.com", sender_name: "CRM News" },
      status: { adapter_registered: true, descriptor_available: true, custody_available: true, manifest_status: "matched", reviewed_pin: null, reported_pin: null, execution_authority: true, network_checked: true },
    }] });
    if (method === "POST" && url.pathname.endsWith("/audience/preview")) {
      return json({
        base: {}, base_count: 5, exclusion_list_id: null, exclusion_count: 1,
        final_excluded: { invalid_email: 0, no_consent: 1, unsubscribed: 0, suppressed: 0 },
        final_count: 3, sample: [{ id: "customer-a", display_name: "Amina" }], digest: "aud-digest-1",
      });
    }
    if (url.pathname.includes("/audience/prepares/")) {
      return json({ freeze: {
        freeze_id: "freeze-1", install_id: "install-crm", context_id: "ctx-a",
        base: { mode: "all" }, exclusion_list_id: null, final_count: 3, max_recipients: 50,
        digest: "aud-digest-1", sample: [],
      }, current_digest: "aud-digest-1", current_final_count: 3, valid: true, drift: null });
    }
    if (url.pathname.endsWith("/segments/list")) return json({ segments: [] });
    if (url.pathname.endsWith("/exclusions/list")) return json({ exclusions: [] });
    if (url.pathname.endsWith("/suppressions/list")) return json({ suppressions: [] });
    if (url.pathname.endsWith("/content/campaigns/list")) return json({ contents: Object.values(contents) });
    if (url.pathname.endsWith("/content/proposals/list")) return json({ proposals: [] });
    if (url.pathname.includes("/content/campaigns/")) {
      const id = url.pathname.split("/").at(-1)!;
      if (contents[id]) return json({ content: contents[id] });
      return refused("email content is unavailable for this installation and context", 404);
    }
    if (path === "/api/app-installations/install-crm") return json(install);
    if (path === "/api/app-installations/install-crm/contexts") return json({ contexts: [
      { id: "ctx-a", install_id: "install-crm", revision: 1, state: "active", digest: "ca", config: { schema: 1, label: "Acme", input_defaults: {} } },
    ] });
    throw new Error(`Unexpected ${method} ${path}`);
  }) as typeof fetch;

  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
  const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
  const text = () => host.textContent ?? "";
  async function settle(check: () => void, budgetMs = 8000) {
    const deadline = Date.now() + budgetMs;
    let last: unknown;
    for (;;) {
      await flush();
      try { check(); return; } catch (e) { last = e; }
      if (Date.now() >= deadline) break;
      await sleep(120);
    }
    await flush();
    try { check(); } catch { throw last; }
  }
  async function click(element: Element | undefined | null) {
    assert(element, "click target exists");
    await React.act(async () => { element!.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
    await flush();
  }
  // CAD-1055: the saved detail is tabbed and mounts only the active
  // panel — open the tab that owns the control a step needs.
  const openTab = async (name: string) => {
    if (host.querySelector(`[data-tab="${name}"][aria-selected="true"]`)) return;
    await click(host.querySelector(`[data-tab="${name}"]`));
  };
  const byText = (tag: string, label: string) =>
    Array.from(host.querySelectorAll(tag)).find((el) => (el.textContent ?? "").trim() === label) ?? null;
  async function fillInput(selector: string, value: string) {
    const element = host.querySelector(selector) as HTMLInputElement;
    assert(element, `field exists: ${selector}`);
    await React.act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(element, value);
      element.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await flush();
  }

  win.sessionStorage.clear();
  history.pushState(null, "", "/app-installations/install-crm?ctx=ctx-a&crm=campaigns&record=launch-1");
  await React.act(async () => {
    root.render(
      React.createElement(AppShell, {
        installId: "install-crm",
        viewer: { operator: true, readOnly: false },
      }),
    );
  });

  // Detail page paints: sender bound, unsubscribe origin, test send
  // armed, and the frozen-audience panel.
  await settle(() => assert(host.querySelector('section[aria-label="Campaign details"]'), "detail renders"));
  await settle(() => assert(text().includes("news@example.com"), "bound sender paints"));
  assert(text().includes("smtp.example.com:465") || text().includes("localhost:465"), "transport paints");
  assert(text().includes("link r1"), "link revision paints");
  assert(text().includes("http://127.0.0.1:3111"), "the effective unsubscribe origin paints");

  // No "locked" copy anywhere.
  assert(!text().includes("locked until"), "no locked-until copy remains");
  assert(!text().includes("locked"), "nothing is described as locked");

  // Prepare is gated on a valid-rechecked freeze — none has been
  // rechecked yet, so the missing list names it.
  const prepareButton = () => byText("button", "Prepare send") as HTMLButtonElement | null;
  assert(prepareButton(), "the prepare control renders");
  await settle(() => assert(prepareButton()!.disabled, "prepare stays gated until the freeze rechecks"));
  assert(
    text().includes("rechecked") && text().includes("missing"),
    "the missing prerequisite is named",
  );

  // An accepted test send of this exact content+binding is required
  // before prepare — run one through the bound sender.
  const sendTestBtn = byText("button", "Send test") as HTMLButtonElement | null;
  assert(sendTestBtn && !sendTestBtn.disabled, "the test send is armed on a live binding");
  await fillInput("#cmp-test-email", "operator@example.com");
  await click(sendTestBtn);
  await settle(() => assert(text().includes("Accepted") || text().includes("accepted"), "the test send receipt paints"));
  assert(text().includes("not proof of inbox delivery"), "the test receipt labels acceptance only");

  // Recheck the freeze — validity lands, prepare unlocks.
  await openTab("audience");
  await click(byText("button", "Recheck freeze"));
  await settle(() => assert(text().includes("Valid"), "freeze validity reports"));
  await openTab("overview");
  await settle(() => {
    const btn = prepareButton();
    if (btn?.disabled) {
      const missing = host.querySelector('[data-prerequisites="missing"]');
      throw new Error(`prepare unlocks once every prerequisite holds — missing: ${missing?.textContent}`);
    }
  });

  // Prepare: counts + masked sample + digest paint.
  await click(prepareButton());
  await settle(() => assert(host.querySelector("[data-prepared]"), "the prepared view paints"));
  assert(text().includes("3 / ceiling 50"), "the final count paints against its ceiling");
  assert(text().includes("a***@example.com"), "the sample stays masked");
  assert(text().includes("freeze-1"), "the frozen audience id paints");
  assert(text().includes("conn-1"), "the sender connection paints");
  const prepareBody = wireBodies.find((row) => row.path === "/api/crm-send/prepare")?.body;
  equal(
    Object.keys(prepareBody ?? {}).sort(),
    ["audience_freeze_id", "campaign_id", "context_id", "install_id", "request_id"],
    "the prepare wire body is exactly the allowlist",
  );

  // Approve demands the typed final count: wrong text keeps the
  // confirm disabled, the right count arms it, and the body is the
  // exact digest-bearing shape.
  await click(byText("button", "Approve and send…"));
  await settle(() => assert(host.querySelector('section[role="dialog"]'), "the approve dialog opens"));
  const confirmApprove = () => byText("button", "Approve and send") as HTMLButtonElement | null;
  assert(confirmApprove()!.disabled, "approve confirm stays disabled until the count is typed");
  await fillInput("#crm-confirm-input", "2");
  assert(confirmApprove()!.disabled, "a wrong count never arms approve");
  await fillInput("#crm-confirm-input", "3");
  assert(!confirmApprove()!.disabled, "the exact final count arms approve");
  await click(confirmApprove());
  await settle(() => assert(text().includes("sending") || text().includes("completed"), "the send's progress paints"));
  const approveBody = wireBodies.find((row) => row.path === "/api/crm-send/approve")?.body;
  equal(
    Object.keys(approveBody ?? {}).sort(),
    ["context_id", "install_id", "send_digest", "send_id"],
    "the approve wire body is exactly the allowlist",
  );

  // Progress: counts paint with the acceptance-only label, the poll
  // stops on `completed`, and the uncertain row alone offers resolve.
  await settle(() => assert(text().includes("completed"), "the send reaches its terminal state"), 30000);
  assert(text().includes("not proof of inbox delivery"), "the acceptance-only label paints");
  const pollsAfterComplete = showPolls;
  await sleep(2300);
  equal(showPolls, pollsAfterComplete, "the poll stops on the terminal state — no busy loop");

  const uncertainRow = host.querySelector('[data-delivery="customer-e"]');
  assert(uncertainRow, "the uncertain row renders");
  const acceptedRow = host.querySelector('[data-delivery="customer-a"]');
  assert(
    acceptedRow && !acceptedRow.textContent!.includes("Mark"),
    "an accepted row never offers resolve",
  );
  await click(
    Array.from(uncertainRow!.querySelectorAll("button")).find((b) => b.textContent === "Mark failed"),
  );
  await settle(() =>
    assert(
      host.querySelector('section[role="dialog"]') &&
        text().includes("no resend happens"),
      "the resolve confirm explains no resend",
    ),
  );
  const confirmDialog = host.querySelector('section[role="dialog"]');
  await click(
    Array.from(confirmDialog!.querySelectorAll("button")).find((b) => b.textContent === "Mark failed"),
  );
  await settle(() => {
    const row = host.querySelector('[data-delivery="customer-e"]');
    assert(row?.textContent?.includes("failed"), "the resolved row reports its outcome");
  });
  const resolveBody = wireBodies.find((row) => row.path === "/api/crm-send/resolve")?.body;
  equal(
    Object.keys(resolveBody ?? {}).sort(),
    ["context_id", "customer_id", "install_id", "resolution", "send_id"],
    "the resolve wire body is exactly the allowlist",
  );

  // Revoke: the binding's confirm dialog removes the sender — the
  // test-send control disables with its reason and prepare re-gates.
  const senderSection = host.querySelector('section[aria-label="SMTP sender"]');
  assert(senderSection, "the sender section renders");
  await click(
    Array.from(senderSection!.querySelectorAll("button")).find((b) => b.textContent === "Revoke binding"),
  );
  await settle(() => assert(host.querySelector('section[role="dialog"]'), "revoke confirm opens"));
  testAccepted = false;
  const revokeDialog = host.querySelector('section[role="dialog"]');
  await click(
    Array.from(revokeDialog!.querySelectorAll("button")).find((b) => b.textContent === "Revoke binding"),
  );
  await settle(() => assert(text().includes("No SMTP sender is bound"), "revocation paints unbound"));
  const testButton = byText("button", "Send test") as HTMLButtonElement | null;
  assert(testButton && testButton.disabled, "test send disables without a binding");
  await settle(() => assert(prepareButton()!.disabled, "prepare re-gates without a sender"));
  assert(text().includes("a live SMTP sender binding"), "the missing sender is named");

  await React.act(async () => { root.unmount(); });

  // CAD-1064: an enrolled SMTP sender whose settings cannot be read
  // is still a sender. The panel says why and links to Connections —
  // it never claims nothing is enrolled.
  const expectations: [string, string][] = [
    ["custody_corrupt", "Rotate it to re-enter"],
    ["withheld_leak", "different password"],
    ["unavailable", "Rotate it to re-enter"],
  ];
  for (const [code, phrase] of expectations) {
    smtpError = code;
    const host2 = document.createElement("div");
    document.body.append(host2);
    const root2 = createRoot(host2);
    win.sessionStorage.clear();
    history.pushState(null, "", "/app-installations/install-crm?ctx=ctx-a&crm=campaigns&record=launch-1");
    await React.act(async () => {
      root2.render(
        React.createElement(AppShell, { installId: "install-crm", viewer: { operator: true, readOnly: false } }),
      );
    });
    const panel2 = () => host2.querySelector('section[aria-label="SMTP sender binding"]');
    await settle(() => {
      const t = panel2()?.textContent ?? "";
      assert(t.includes("can't be used yet"), `${code}: names the unusable sender`);
      assert(t.includes(phrase), `${code}: gives the fix (${phrase})`);
    });
    const t2 = panel2()!.textContent!;
    assert(!t2.includes("No enrolled SMTP connections"), `${code}: never says nothing is enrolled`);
    const link = panel2()!.querySelector('a[href="/settings/connections"]');
    assert(link, `${code}: links to Settings → Connections`);
    await React.act(async () => { root2.unmount(); });
    host2.remove();
  }
  smtpError = null;

  console.log("crm send checks passed");
}

void main();
