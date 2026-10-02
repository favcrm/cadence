export {};
/**
 * CAD-784 CRM Campaigns: content wire grammar plus mounted
 * list/new/detail flow through the real AppShell — no inline builder
 * on the list, host previews, distinct test-send, attributed
 * Apply/Discard, and locked preview-only sender material.
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

async function main() {
  await contentGrammar();
  await mountedFlow();
}

async function contentGrammar() {
  const { contentClient } = require("../src/features/app-shell/contentClient") as typeof import(
    "../src/features/app-shell/contentClient"
  );
  const { audienceClient, audiencePaths } = require("../src/features/app-shell/audienceClient") as typeof import(
    "../src/features/app-shell/audienceClient"
  );
  const { ApiError } = require("../src/lib/api") as typeof import("../src/lib/api");
  const scope = { installId: "install-a", contextId: "ctx-b" };
  equal(
    audiencePaths.exclusionSavePath(scope),
    "/api/app-installations/install-a/contexts/ctx-b/exclusions",
    "exclusion save binds the URL scope",
  );
  equal(
    audiencePaths.suppressionAddPath(scope),
    "/api/app-installations/install-a/contexts/ctx-b/suppressions",
    "suppression add binds the URL scope",
  );
  equal(
    audiencePaths.preparePath(scope),
    "/api/app-installations/install-a/contexts/ctx-b/audience/prepares",
    "freeze prepare binds the URL scope",
  );
  // Receipt-shaped fields refuse on every content/audience write: no
  // assistant seam exists yet, so any present receipt is forged.
  const realFetch = (globalThis as any).fetch;
  (globalThis as any).fetch = () => {
    throw new Error("network must not be reached by a receipt-bearing body");
  };
  try {
    for (const [call, why] of [
      [
        () =>
          (contentClient as any).propose(scope, {
            campaignId: "launch-1",
            proposalId: "prop-1",
            subject: "S",
            blocks: [],
            assistant_receipt: { turn_id: "t" },
          }),
        "proposed receipt",
      ],
      [
        () =>
          (contentClient as any).save(scope, {
            campaignId: "launch-1",
            subject: "S",
            blocks: [],
            turn_id: "t",
          }),
        "save turn claim",
      ],
      [
        () =>
          (audienceClient as any).segmentSave(scope, {
            segmentId: "seg-1",
            name: "N",
            predicates: [],
            nonce: "n",
          }),
        "segment nonce",
      ],
    ] as [() => Promise<unknown>, string][]) {
      let refused = false;
      try {
        await call();
      } catch (error) {
        assert(error instanceof ApiError, `${why} refuses typed`);
        refused = true;
      }
      assert(refused, `${why} reached the network`);
    }
  } finally {
    (globalThis as any).fetch = realFetch;
  }

  // The constrained editor grammar mirrors the host bounds.
  const grammar = require("../src/features/app-shell/campaignGrammar") as typeof import(
    "../src/features/app-shell/campaignGrammar"
  );
  grammar.checkContent("Hello {{first_name|Friend}}", "", [{ type: "paragraph", text: "Hi" }]);
  grammar.checkContent("S", "", [{ type: "button", label: "Shop", url: "https://example.com/offer" }]);
  for (const [subject, preheader, blocks, why] of [
    ["", "", [{ type: "paragraph", text: "Hi" }], "blank subject"],
    ["S", "", [], "no blocks"],
    ["S", "", [{ type: "heading", text: "a<b" }], "markup paste"],
    ["S {{other|x}}", "", [{ type: "paragraph", text: "Hi" }], "unknown token"],
    ["S {{first_name|}}", "", [{ type: "paragraph", text: "Hi" }], "empty fallback"],
    ["S", "", [{ type: "button", label: "Go", url: "http://example.com" }], "plain http"],
    ["S", "", [{ type: "button", label: "Go", url: "javascript:alert(1)" }], "script url"],
    ["S", "", [{ type: "video", text: "x" }], "unknown block"],
  ] as [string, string, any[], string][]) {
    let refused = false;
    try {
      grammar.checkContent(subject, preheader, blocks);
    } catch (error) {
      assert(error instanceof ApiError, `${why} refuses typed`);
      refused = true;
    }
    assert(refused, `${why} passed the client grammar`);
  }
  try {
    grammar.checkCampaignId("bad id");
    throw new Error("bad campaign id passed");
  } catch (error) {
    assert(error instanceof ApiError, "bad campaign id refuses typed");
  }

  // CAD-813: the mint path — request receipt parsing, identifier-safe
  // request ids, and the scoped-message picker that decides what
  // `message_id` may be named.
  const stamped = grammar.parseProposalRequest({
    request: {
      request_id: "req-1", install_id: "install-crm", context_id: "ctx-a",
      campaign_id: "launch-1", source_revision: 3, message_id: "chat-9",
      state: "open", used_by: null, created: 1759286400, decided: null,
    },
  });
  equal(stamped.campaignId, "launch-1", "mint receipt carries the stamped campaign");
  equal(stamped.sourceRevision, 3, "mint receipt carries the stamped source revision");
  equal(stamped.state, "open", "mint receipt carries its state");
  let refusedReceipt = false;
  try {
    grammar.parseProposalRequest({ request: { request_id: "req-1" } });
  } catch (error) {
    assert(error instanceof ApiError, "a malformed request receipt refuses typed");
    refusedReceipt = true;
  }
  assert(refusedReceipt, "a malformed request receipt reached the model");
  const reqId = grammar.newRequestId();
  assert(/^req-[0-9a-f]{24}$/.test(reqId), `request id is identifier-safe: ${reqId}`);
  assert(grammar.newRequestId() !== reqId, "request ids are fresh");

  // The mint body over the wire is exactly {campaign_id, message_id,
  // request_id}: a forged field on the input refuses client-side.
  const wireBodies: any[] = [];
  const realFetch2 = (globalThis as any).fetch;
  (globalThis as any).fetch = async (_input: unknown, init?: RequestInit) => {
    wireBodies.push(JSON.parse(String(init?.body)));
    return new Response(JSON.stringify({ request: {
      request_id: "req-w", install_id: "install-crm", context_id: "ctx-a",
      campaign_id: "launch-1", source_revision: 1, message_id: "chat-9",
      state: "open", used_by: null, created: 1, decided: null,
    } }), { status: 200, headers: { "Content-Type": "application/json" } });
  };
  try {
    await contentClient.proposalRequest(scope, {
      campaignId: "launch-1", messageId: "chat-9", requestId: "req-w",
    });
    equal(
      Object.keys(wireBodies[0]).sort(),
      ["campaign_id", "message_id", "request_id"],
      "the mint body is exactly the three ids — no token/receipt/turn/source_revision",
    );
    let forged = false;
    try {
      await (contentClient as any).proposalRequest(scope, {
        campaignId: "launch-1", messageId: "chat-9", requestId: "req-w2",
        source_revision: 7, token: "t", assistant_receipt: {}, turn_id: "t",
      });
    } catch (error) {
      assert(error instanceof ApiError, "a forged mint field refuses typed");
      forged = true;
    }
    assert(forged, "a forged mint field reached the network");
    assert(wireBodies.length === 1, "the forged mint never reached fetch");
  } finally {
    (globalThis as any).fetch = realFetch2;
  }

  // The proposal render rule: verified badge needs actor==assistant
  // AND a parsed receipt; anything else is operator-submitted.
  const receipted = grammar.parseProposal({ proposal: {
    proposal_id: "prop-a", campaign_id: "launch-1", source_revision: 1,
    subject: "S", state: "pending", actor: "assistant", origin: "assistant-receipt",
    assistant_receipt: {
      message_id: "m-scoped", agent: "crm-writer", request_id: "req-1",
      install_id: "install-crm", context_id: "ctx-a", campaign_id: "launch-1",
      source_revision: 1,
    },
  } });
  equal(receipted.assistantReceipt?.agent, "crm-writer", "receipt parses its agent");
  equal(receipted.assistantReceipt?.requestId, "req-1", "receipt parses its request id");
  const noReceipt = grammar.parseProposal({ proposal: {
    proposal_id: "prop-o", campaign_id: "launch-1", source_revision: 1,
    subject: "S", state: "pending", actor: "assistant", origin: "assistant-receipt",
    assistant_receipt: null,
  } });
  equal(noReceipt.assistantReceipt, null, "a null receipt stays null — never assistant-badged");
  const malformedReceipt = grammar.parseProposal({ proposal: {
    proposal_id: "prop-m", campaign_id: "launch-1", source_revision: 1,
    subject: "S", state: "pending", actor: "assistant", origin: "assistant-receipt",
    assistant_receipt: { agent: "crm-writer" },
  } });
  equal(malformedReceipt.assistantReceipt, null, "a malformed receipt drops to no-provenance");
}

async function mountedFlow() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/app-installations/install-crm?ctx=ctx-a" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLSelectElement", "HTMLIFrameElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener"])
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
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

  // CAD-813: `latestScopedChatMessage` picks only the newest operator
  // entry whose daemon stamp names exactly this scope — pending sends,
  // foreign scopes and unverified payloads never qualify.
  {
    const { latestScopedChatMessage } = require("../src/features/app-shell/AppShell") as typeof import(
      "../src/features/app-shell/AppShell"
    );
    const verifiedPayload = {
      app: { install_id: "install-crm", context_id: "ctx-a", verified: true, context_revision: 1, context_digest: "ca" },
    };
    const foreignPayload = {
      app: { install_id: "install-crm", context_id: "other-ctx", verified: true, context_revision: 1, context_digest: "x" },
    };
    const entries = [
      { seq: 1, role: "operator", kind: "message", text: "a", message: "m-plain", payload: null },
      { seq: 2, role: "operator", kind: "message", text: "b", message: "m-foreign", payload: foreignPayload },
      { seq: 3, role: "operator", kind: "message", text: "c", message: "m-scoped", payload: verifiedPayload },
      { seq: 4, role: "agent", kind: "turn_result", text: "d", message: "m-agent", payload: verifiedPayload },
    ];
    equal(
      latestScopedChatMessage({ entries }, { installId: "install-crm", contextId: "ctx-a" }),
      "m-scoped",
      "the newest verified-scoped operator message is picked",
    );
    equal(
      latestScopedChatMessage({ entries }, { installId: "install-crm", contextId: "other-ctx" }),
      "m-foreign",
      "a message scoped to another context mints there, not here",
    );
    equal(
      latestScopedChatMessage({ entries }, { installId: "install-crm", contextId: "missing" }),
      null,
      "no scoped message means no mintable id",
    );
    equal(
      latestScopedChatMessage({ entries }, { installId: "install-crm", contextId: "" }),
      null,
      "an empty context never mints",
    );
    const unverified = [{ seq: 1, role: "operator", kind: "message", text: "x", message: "m-un", payload: { app: { install_id: "install-crm", context_id: "ctx-a", verified: false } } }];
    equal(
      latestScopedChatMessage({ entries: unverified }, { installId: "install-crm", contextId: "ctx-a" }),
      null,
      "an unverified app stamp never mints",
    );
  }

  const React = require("react") as typeof import("react");
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const AppShell = (require("../src/features/app-shell/AppShell") as typeof import("../src/features/app-shell/AppShell")).default;

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
      approval: { revision: null, digest: null, valid: false, scope: "content-only" },
    },
  };
  const proposals: Record<string, any> = {};
  const proposalRequests: Record<string, any> = {};
  const mintBodies: any[] = [];
  const applyBodies: any[] = [];
  const sendBodies: any[] = [];
  let propSeq = 0;
  // The assistant's turn lands later than the mint: `redeem` plays the
  // daemon-side redemption (the socket-only verb a browser can never
  // reach) so the list starts answering with a receipted proposal.
  const redeem = (requestId: string) => {
    const request = proposalRequests[requestId];
    assert(request && request.state === "open", `redeem needs an open request: ${requestId}`);
    propSeq += 1;
    const id = `prop-asst-${propSeq}`;
    proposals[id] = {
      proposal_id: id, install_id: "install-crm", context_id: "ctx-a",
      campaign_id: request.campaign_id, source_revision: request.source_revision,
      subject: "Assistant draft subject", preheader: "From the chat turn",
      blocks: [{ type: "paragraph", text: "Assistant copy {{first_name|Friend}}" }],
      content_digest: `proposal-digest-asst-${propSeq}`, actor: "assistant",
      origin: "assistant-receipt",
      assistant_receipt: {
        message_id: request.message_id, agent: "crm-writer", request_id: request.request_id,
        install_id: "install-crm", context_id: "ctx-a",
        campaign_id: request.campaign_id, source_revision: request.source_revision,
      },
      state: "pending", created: 1759286400, decided: null,
    };
    request.state = "used";
    request.used_by = id;
    return proposals[id];
  };
  const suppressions: { kind: string; key: string; reason: string }[] = [];
  const threadEntries: any[] = [];
  let threadSeq = 0;
  // What the daemon would append for an accepted scoped thread_send:
  // the verified App stamp is host-derived, the browser's `app` body
  // value is only the hint it proves against the store.
  const stampEntry = (body: any) => {
    threadSeq += 1;
    const entry = {
      seq: threadSeq, role: "operator", kind: "message", text: body.text,
      message: body.message, created: "2026-09-29T00:00:00Z",
      payload: body.app
        ? { app: {
            install_id: body.app.install_id, context_id: body.app.context_id,
            verified: true, context_revision: 1, context_digest: "ca",
          } }
        : null,
    };
    threadEntries.push(entry);
    return entry;
  };
  const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
  const refused = (message: string, status = 409) => json({ error: message }, status);
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const path = String(input);
    const url = new URL(path, "http://localhost");
    const method = init?.method ?? "GET";
    const parts = url.pathname.split("/");
    // Content saves ride the collection path with CAS.
    if (method === "POST" && url.pathname.endsWith("/content/campaigns")) {
      const body = JSON.parse(String(init!.body));
      const current = contents[body.campaign_id]?.revision as number | undefined;
      if (current !== undefined && body.expected_revision === undefined) {
        return refused("email content already exists; name the observed revision");
      }
      if (current !== undefined && body.expected_revision !== current) {
        return refused("email content revision is stale");
      }
      const revision = current === undefined ? 1 : current + 1;
      contents[body.campaign_id] = {
        campaign_id: body.campaign_id, install_id: "install-crm", context_id: "ctx-a", revision,
        subject: body.subject, preheader: body.preheader, blocks: body.blocks,
        content_digest: `content-digest-${revision}`,
        approval: { revision: null, digest: null, valid: false, scope: "content-only" },
      };
      return json({ content: contents[body.campaign_id] });
    }
    if (method === "POST" && parts.includes("campaigns") && parts.at(-1) === "render") {
      const id = parts.at(-2)!;
      const doc = contents[id];
      if (!doc) return refused("email content is unavailable for this installation and context", 404);
      return json({ render: {
        campaign_id: id, install_id: "install-crm", context_id: "ctx-a", revision: doc.revision,
        content_digest: doc.content_digest, sample_first_name: null,
        binding: { binding_id: "preview", revision: 1, digest: "binding-digest", preview_only: true },
        preview_only: true, send_ready: false,
        sender: { name: "Cadence CRM", address: "noreply@cadence.invalid" },
        unsubscribe_url: "https://cadence.invalid/unsubscribe/preview",
        html: "<h1>HTML form</h1>", text: "TEXT form", render_digest: "render-digest-1",
      } });
    }
    if (method === "POST" && parts.includes("campaigns") && parts.at(-1) === "approve") {
      const id = parts.at(-2)!;
      const doc = contents[id];
      const body = JSON.parse(String(init!.body));
      if (!doc) return refused("email content is unavailable for this installation and context", 404);
      if (body.expected_revision !== doc.revision) return refused("email content revision is stale");
      doc.approval = { revision: doc.revision, digest: doc.content_digest, valid: true, scope: "content-only" };
      return json({ content: doc });
    }
    if (method === "POST" && parts.includes("campaigns") && parts.at(-1) === "test-prepare") {
      const id = parts.at(-2)!;
      const doc = contents[id];
      const body = JSON.parse(String(init!.body));
      if (!doc) return refused("email content is unavailable for this installation and context", 404);
      if (typeof body.to_email !== "string" || !body.to_email.includes("@")) {
        return refused("email test recipient exceeds its supported shape or bounds");
      }
      return json({ test_send: {
        kind: "test", campaign_id: id, install_id: "install-crm", context_id: "ctx-a",
        content_revision: doc.revision, content_digest: doc.content_digest,
        sender_binding: { binding_id: "preview", preview_only: true },
        preview_only: true, send_ready: false, to_email: body.to_email,
        payload_digest: "payload-digest-1",
      } });
    }
    // CAD-813: the one-time proposal-request mint — the body names
    // only campaign/message/request ids; the host stamps the source
    // revision and binds the chat message's verified scope.
    if (method === "POST" && url.pathname.endsWith("/content/proposal-requests")) {
      const body = JSON.parse(String(init!.body));
      mintBodies.push(body);
      const entry = threadEntries.find((row) => row.message === body.message_id);
      const bound = entry?.payload?.app;
      if (!entry || bound?.install_id !== "install-crm" || bound?.context_id !== "ctx-a") {
        return refused("proposal request scope does not match its verified chat message", 409);
      }
      const request = proposalRequests[body.request_id];
      if (request) {
        if (request.campaign_id === body.campaign_id && request.message_id === body.message_id) {
          return json({ request });
        }
        return refused("email proposal request ID is already used", 409);
      }
      proposalRequests[body.request_id] = {
        request_id: body.request_id, install_id: "install-crm", context_id: "ctx-a",
        campaign_id: body.campaign_id,
        source_revision: contents[body.campaign_id]?.revision ?? 0,
        message_id: body.message_id, state: "open", used_by: null,
        created: 1759286400, decided: null,
      };
      return json({ request: proposalRequests[body.request_id] });
    }
    if (method === "POST" && url.pathname.endsWith("/content/proposals")) {
      const body = JSON.parse(String(init!.body));
      propSeq += 1;
      const id = body.proposal_id as string;
      proposals[id] = {
        proposal_id: id, install_id: "install-crm", context_id: "ctx-a",
        campaign_id: body.campaign_id, source_revision: contents[body.campaign_id]?.revision ?? 0,
        subject: body.subject, preheader: body.preheader, blocks: body.blocks,
        content_digest: `proposal-digest-${propSeq}`, actor: "operator", origin: "operator-direct",
        assistant_receipt: null, state: "pending", created: 1759286400, decided: null,
      };
      return json({ proposal: proposals[id] });
    }
    if (method === "POST" && parts.includes("proposals") && parts.at(-1) === "apply") {
      const id = parts.at(-2)!;
      const row = proposals[id];
      if (!row || row.state !== "pending") return refused("email proposal is already decided");
      const body = String(init!.body ?? "");
      applyBodies.push(body === "" ? {} : JSON.parse(body));
      const expected = body === "" ? undefined : (JSON.parse(body).expected_revision as number | undefined);
      const doc = contents[row.campaign_id];
      if (expected !== undefined && doc && expected !== doc.revision) {
        return refused("email content revision is stale");
      }
      if (doc && row.source_revision !== doc.revision) return refused("email proposal source revision is stale");
      const revision = (doc?.revision ?? 0) + 1;
      contents[row.campaign_id] = {
        ...doc, revision, subject: row.subject, preheader: row.preheader, blocks: row.blocks,
        content_digest: `content-digest-${revision}`,
        approval: { revision: null, digest: null, valid: false, scope: "content-only" },
      };
      row.state = "applied";
      return json({ content: contents[row.campaign_id] });
    }
    if (method === "POST" && parts.includes("proposals") && parts.at(-1) === "discard") {
      const id = parts.at(-2)!;
      const row = proposals[id];
      if (!row || row.state !== "pending") return refused("email proposal is already decided");
      row.state = "discarded";
      return json({ proposal: row });
    }
    if (method === "POST" && url.pathname.endsWith("/audience/preview")) {
      return json({
        base: {}, base_count: 5, exclusion_list_id: null, exclusion_count: 0,
        final_excluded: { invalid_email: 0, no_consent: 0, unsubscribed: 0, suppressed: suppressions.length },
        final_count: 5,
        sample: [{ id: "customer-s1", display_name: "Seed Alpha" }],
        digest: "audience-digest-1",
      });
    }
    if (method === "POST" && url.pathname.endsWith("/audience/prepares")) {
      const body = JSON.parse(String(init!.body));
      return json({ freeze: {
        freeze_id: body.freeze_id, install_id: "install-crm", context_id: "ctx-a",
        base: body.base, exclusion_list_id: body.exclusion_list_id ?? null,
        final_count: 5, max_recipients: body.max_recipients,
        digest: "audience-digest-1", sample: [{ id: "customer-s1", display_name: "Seed Alpha" }],
        replayed: false,
      } });
    }
    if (method === "POST" && url.pathname.endsWith("/suppressions")) {
      const body = JSON.parse(String(init!.body));
      suppressions.push({ kind: "email", key: body.email as string, reason: body.reason as string });
      return json({ suppression: suppressions.at(-1) });
    }
    // CAD-785/786: the flat send routes — a live binding is always
    // bound, the test send accepts, and no sends exist yet.
    if (method === "POST" && url.pathname === "/api/crm-smtp/show") {
      return json({ binding: {
        install_id: "install-crm", context_id: "ctx-a",
        connection_id: "conn-smtp-1", auth_revision: 1, link_revision: 1,
        state: "live", digest: "sha256:linkdigest1",
        sender: { name: "CRM News", address: "news@example.com" },
        transport: { host: "smtp.example.com", port: 465, tls_mode: "implicit", username: "smtp-user" },
      } });
    }
    if (method === "POST" && url.pathname === "/api/crm-smtp/test-send") {
      const body = JSON.parse(String(init!.body));
      const doc = contents[body.campaign_id];
      if (!doc) return refused("email content is unavailable for this installation and context", 404);
      return json({ receipt: {
        test_send: true, kind: "test", campaign_id: body.campaign_id,
        to_email: body.to_email, accepted: true, smtp_code: 250, smtp_message: "accepted",
        content_revision: doc.revision, content_digest: doc.content_digest,
        link_digest: "sha256:linkdigest1", delivery_claim: "smtp-acceptance-only",
      } });
    }
    if (url.pathname === "/api/crm-send/list") return json({ sends: [] });
    if (url.pathname === "/api/crm-send/origin") {
      if (method === "POST") {
        const body = JSON.parse(String(init!.body));
        return json({ unsubscribe_origin: body.unsubscribe_origin });
      }
      return json({ unsubscribe_origin: "https://cadence.invalid", stored: false });
    }
    if (path === "/api/connections") return json({ connections: [{
      id: "conn-smtp-1", provider: "smtp", account: "isolated", kind: "enrolled",
      revision: 1, registration_digest: "sha256:reg1", scopes: ["email:send"],
      smtp: { host: "smtp.example.com", port: 465, tls_mode: "implicit", username: "smtp-user", sender: "news@example.com", sender_name: "CRM News" },
      status: { adapter_registered: true, descriptor_available: true, custody_available: true, manifest_status: "matched", reviewed_pin: null, reported_pin: null, execution_authority: true, network_checked: true },
    }] });
    if (path === "/api/app-installations/install-crm") return json(install);
    if (path === "/api/app-installations/install-crm/contexts") return json({ contexts: [
      { id: "ctx-a", install_id: "install-crm", revision: 1, state: "active", digest: "ca", config: { schema: 1, label: "Acme", input_defaults: {} } },
    ] });
    // The shared master thread: sends append a verified-scope entry
    // exactly as the daemon stamps them, and page reads replay it.
    if (method === "POST" && url.pathname === "/api/threads/master/messages") {
      const body = JSON.parse(String(init!.body));
      sendBodies.push(body);
      const entry = stampEntry(body);
      return json({ message: entry.message, state: "queued", duplicate: false });
    }
    if (path.startsWith("/api/threads/master")) {
      return json({ entries: threadEntries, more_before: false });
    }
    if (url.pathname.endsWith("/segments/list")) return json({ segments: [] });
    if (url.pathname.endsWith("/exclusions/list")) return json({ exclusions: [] });
    if (url.pathname.endsWith("/suppressions/list")) return json({ suppressions });
    if (url.pathname.endsWith("/content/campaigns/list")) return json({ contents: Object.values(contents) });
    if (url.pathname.endsWith("/content/proposals/list")) return json({ proposals: Object.values(proposals) });
    if (url.pathname.includes("/content/campaigns/")) {
      const id = parts.at(-1)!;
      if (contents[id]) return json({ content: contents[id] });
      return refused("email content is unavailable for this installation and context", 404);
    }
    if (url.pathname.includes("/audience/prepares/")) {
      return json({ freeze: {
        freeze_id: parts.at(-1), install_id: "install-crm", context_id: "ctx-a",
        base: { mode: "all" }, exclusion_list_id: null, final_count: 5, max_recipients: 500,
        digest: "audience-digest-1", sample: [{ id: "customer-s1", display_name: "Seed Alpha" }],
      }, current_digest: "audience-digest-1", current_final_count: 5, valid: true, drift: null });
    }
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
    await React.act(async () => { element.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
    await flush();
  }
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
  async function fillArea(selector: string, value: string) {
    const element = host.querySelector(selector) as HTMLTextAreaElement;
    assert(element, `area exists: ${selector}`);
    await React.act(async () => {
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(element, value);
      element.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await flush();
  }

  win.sessionStorage.clear();
  history.pushState(null, "", "/app-installations/install-crm?ctx=ctx-a&crm=campaigns");
  await React.act(async () => {
    root.render(React.createElement(AppShell, { installId: "install-crm", viewer: { operator: true, readOnly: false } }));
  });
  // The list shows status per saved campaign and contains no builder.
  await settle(() => assert(text().includes("Hello"), "campaigns list paints server rows"));
  assert(text().includes("Draft"), "list shows the content-only approval state");
  const listSection = host.querySelector('section[aria-label="Campaigns list"]');
  assert(listSection, "campaigns list section renders");
  assert(!listSection!.querySelector("input, textarea"), "the list contains no inline builder fields");
  assert(!text().includes("Create campaign"), "the list contains no inline builder submit");

  // Page URL transitions exercise the mounted shell; actual sidebar/mobile
  // anchors and current-page state are covered by crmAppMenu.test.
  const { crmSectionHref } = require("../src/features/app-shell/CrmOutlet") as typeof import("../src/features/app-shell/CrmOutlet");
  const { navigate } = require("../src/lib/useLocation") as typeof import("../src/lib/useLocation");
  async function openPage(section: "customers" | "segments" | "campaigns") {
    await React.act(async () => { navigate(crmSectionHref(location.pathname + location.search, section)); });
    await flush();
  }
  await openPage("segments");
  await settle(() => assert(location.search.includes("crm=segments"), "segments URL routes"));
  await openPage("customers");
  await settle(() => assert(!location.search.includes("crm="), "customers is the default route"));
  await openPage("campaigns");
  await settle(() => assert(location.search.includes("crm=campaigns"), "campaigns URL routes back"));
  // Switching sections clears the record view: no drawer follows.
  assert(!host.querySelector("[data-drawer]"), "no drawer follows a section switch");

  // New campaign: audience preview counts render live without a save.
  await click(byText("button", "New campaign"));
  await settle(() => assert(host.querySelector('[data-state="no-draft"]'), "new campaign page opens in the no-draft preview-first state"));
  await React.act(async () => { await sleep(700); });
  await settle(() => assert(text().includes("Final recipients"), "audience preview counts render pre-save"));
  // Suppression add refreshes the suppression counts.
  await fillInput("#aud-suppress-email", "gone@example.com");
  await fillInput("#aud-suppress-reason", "opted out by phone");
  await click(byText("button", "Add suppression"));
  await settle(() => assert(text().includes("gone@example.com"), "added suppression lists"));
  // Freeze the picked audience under an operator-named ID.
  await click(byText("button", "Freeze audience"));
  await settle(() => assert(text().includes("Frozen") && text().includes("5 recipients"), "freeze receipt reports"));

  // Content save unlocks preview, test-send and proposals in place.
  // CAD-1013 preview-first: there is no manual composer — the campaign
  // starts with "No email draft yet" and is created by the assistant
  // proposal path, never a bare Content form.
  assert(host.querySelector('[data-state="no-draft"]'), "unsaved campaign shows the no-draft state, not a composer");
  assert(!host.querySelector('form[aria-label="Email content"]'), "no manual block editor renders");
  assert(!byText("button", "Submit editor as proposal (operator-submitted)"), "the operator proposal shortcut is gone");
  assert(!host.querySelector("#cmp-subject"), "no manual subject input until a draft exists");

  // Initial creation is not stranded: send the assistant a scoped chat
  // message, mint a proposal request (source r0), redeem it, Apply —
  // that creates revision 1 through the verified assistant path.
  const mintButton0 = () => byText("button", "Ask assistant to draft") as HTMLButtonElement | null;
  assert(mintButton0(), "the assistant-draft mint control renders pre-save");
  assert(mintButton0()!.disabled, "mint is disabled without a scoped chat message");
  await fillArea("#app-shell-chat-box", "Draft the launch email for this campaign");
  await click(byText("button", "Send"));
  await React.act(async () => {
    const { resources } = require("../src/lib/resources") as typeof import("../src/lib/resources");
    await resources.masterThread.refresh();
  });
  await settle(() => assert(!mintButton0()!.disabled, "mint enables once a scoped message exists"));
  await click(mintButton0());
  await settle(() => assert(mintBodies.length === 1, "one proposal request minted pre-save"));
  assert(mintBodies[0].campaign_id, "mint names the campaign");
  redeem(mintBodies[0].request_id as string);
  await settle(() => assert(text().includes("Verified assistant draft"), "the verified proposal lands pre-save"), 30000);
  const firstLi = host.querySelector('[data-proposal^="prop-asst-"]');
  assert(firstLi, "the verified proposal row exists");
  await click(Array.from(firstLi!.querySelectorAll("button")).find((b) => (b.textContent ?? "").includes("Apply")));
  await settle(() => assert(text().includes("Applied as revision 1"), "the verified proposal creates revision 1"));
  const openDoc = () => contents[(host.querySelector("#cmp-id") as HTMLInputElement | null)?.value ?? ""]
    ?? Object.values(contents).at(-1);
  assert(openDoc().revision === 1, "revision 1 exists after the assistant apply");

  // Host-rendered preview (CAD-1008): the saved revision renders
  // automatically — Visual default, HTML/Text tabs — beside the content
  // summary card. A manual Refresh re-renders the same saved version.
  await settle(() => {
    const frame = host.querySelector('iframe[data-preview="visual"]') as HTMLIFrameElement;
    assert(frame && (frame.getAttribute("srcdoc") ?? "").includes("HTML form"), "the saved revision auto-rendered");
  });
  await click(byText("button", "Refresh preview"));
  await settle(() => {
    const frame = host.querySelector('iframe[data-preview="visual"]') as HTMLIFrameElement;
    assert(frame && (frame.getAttribute("srcdoc") ?? "").includes("HTML form"), "refresh re-renders the saved revision");
  });
  await click(byText("button", "Text"));
  await flush();
  assert((host.querySelector('pre[data-preview="text"]')?.textContent ?? "").includes("TEXT form"), "text tab shows the host text form");
  assert(text().includes("noreply@cadence.invalid"), "preview-only sender material renders");

  // Bounded inline text correction: Edit subject / text opens the only
  // operator edit — subject, preheader and existing block text, never
  // block type, URL, token tooling or add/remove — and saves a new
  // revision through expectedRevision (a conflict keeps local text).
  await click(byText("button", "Edit subject / text"));
  await settle(() => assert(host.querySelector("#cmp-subject"), "inline edit reveals subject"));
  assert(!host.querySelector('[aria-label="Add block"]'), "no add-block control in inline edit");
  await fillInput("#cmp-subject", "Assistant draft subject v2");
  await click(byText("button", "Save text corrections (new revision)"));
  await settle(() => assert(text().includes("Saved revision 2"), "the text correction saved a new revision"));

  // Approval is content-only; a proposal Apply invalidates it and a
  // stale-source proposal refuses with Apply disabled.
  await click(byText("button", "Approve r2 (content-only)"));
  await settle(() => assert(text().includes("Approved r2"), "approval lands content-only"));
  // Mint a second assistant proposal at r2, then save r3 via a text
  // correction so the proposal drifts stale.
  await click(mintButton0());
  await settle(() => assert(mintBodies.length === 2, "a second proposal request mints"));
  redeem(mintBodies[1].request_id as string);
  await settle(() => assert(host.querySelectorAll('[data-proposal]').length > 0, "second proposal pends"), 30000);
  await click(byText("button", "Edit subject / text"));
  await settle(() => assert(host.querySelector("#cmp-subject"), "inline edit reopens"));
  await fillInput("#cmp-subject", "Assistant draft subject v3");
  await click(byText("button", "Save text corrections (new revision)"));
  await settle(() => assert(text().includes("Saved revision 3"), "the correction saved r3"));
  await settle(() => assert(text().includes("Needs review (stale)"), "the drifted proposal renders stale"));
  const staleApply = Array.from(host.querySelectorAll('[data-proposal]'))
    .map((li) => li.querySelector("button"))
    .find((b) => (b?.textContent ?? "").includes("Apply"));
  assert(staleApply && (staleApply as HTMLButtonElement).disabled, "stale proposal's Apply is disabled");
  // Discard clears it non-mutating.
  await click(byText("button", "Discard"));
  await settle(() => assert(text().includes("draft unchanged at r3"), "discard is non-mutating"));

  // ---- CAD-1013 pinned inline edit ----
  // While the operator edits text, a concurrent agent Apply lands a
  // newer revision. The editor must keep the local text, flag the edit
  // stale and pin the save to the revision it began on — a conflict
  // refuses instead of silently overwriting the agent's newer draft.
  await click(byText("button", "Edit subject / text"));
  await settle(() => assert(host.querySelector("#cmp-subject"), "inline edit opens for the pinned test"));
  const pinnedBase = openDoc().revision as number;
  await fillInput("#cmp-subject", "My kept local correction");
  // A concurrent assistant proposal lands and applies → doc r(N+1).
  const mintsBefore = mintBodies.length;
  await click(mintButton0());
  await settle(() => assert(mintBodies.length === mintsBefore + 1, "a fresh request minted mid-edit"), 15000);
  redeem(mintBodies.at(-1)!.request_id as string);
  await settle(() => assert(host.querySelectorAll('[data-proposal]').length > 0, "a fresh proposal pends"), 30000);
  const freshLi = Array.from(host.querySelectorAll('[data-proposal]')).at(-1)!;
  await click(Array.from(freshLi.querySelectorAll("button")).find((b) => (b.textContent ?? "").includes("Apply")));
  await settle(() => assert(openDoc().revision === pinnedBase + 1, "a concurrent apply bumped the doc mid-edit"));
  // Local text is preserved and the stale flag shows.
  assert(
    (host.querySelector("#cmp-subject") as HTMLInputElement).value === "My kept local correction",
    "the concurrent apply did not clobber the local edit",
  );
  assert(host.querySelector('[data-stale-edit]'), "the stale-edit warning renders");
  // Saving stays pinned to the base revision → the backend conflicts.
  await click(byText("button", "Save text corrections (new revision)"));
  await settle(() =>
    assert(
      text().includes("revision is stale") || text().includes("moved since you started editing"),
      "the pinned save conflicts instead of overwriting the newer draft",
    ),
  );
  assert(
    (host.querySelector("#cmp-subject") as HTMLInputElement).value === "My kept local correction",
    "the refused save keeps the local text for a reload decision",
  );
  await click(byText("button", "Cancel"));

  // ---- CAD-813: the verified assistant draft seam ----
  // No scoped chat message yet: the mint control stays disabled with
  // its explanation, and no request body ever leaves the browser.
  const mintButton = () => byText("button", "Ask assistant to draft") as HTMLButtonElement | null;
  assert(mintButton(), "the assistant-draft mint control renders");
  assert(mintButton()!.disabled, "mint is disabled without a scoped chat message");
  assert(
    text().includes("Send the assistant a message in the left chat first"),
    "the disabled mint explains what to do",
  );
  assert(mintBodies.length === 0, "no proposal request left the browser yet");

  // Send the assistant a scoped chat message through the left pane —
  // the daemon stamps the verified App binding on the stored entry,
  // which the stream then lands in the shared thread store.
  await fillArea("#app-shell-chat-box", "Draft the launch email for this campaign");
  await click(byText("button", "Send"));
  assert(sendBodies.length === 1, "the left chat sent one message");
  equal(
    sendBodies[0].app,
    { install_id: "install-crm", context_id: "ctx-a" },
    "the chat send carried the shell's scope",
  );
  // The stored entry lands via the thread stream; in this fixture the
  // fake SSE is silent, so revalidate the shared store the way the
  // stream's arrival would.
  await React.act(async () => {
    const { resources } = require("../src/lib/resources") as typeof import("../src/lib/resources");
    await resources.masterThread.refresh();
  });
  await settle(() => assert(!mintButton()!.disabled, "the mint enables once a scoped message exists"));

  // Mint: the body is exactly {campaign_id, message_id, request_id} —
  // no token, receipt, turn or source_revision ever travels. (This is
  // the third mint: two earlier ones already ran in this scenario.)
  const mintCountBefore = mintBodies.length;
  await click(mintButton());
  await settle(() => assert(mintBodies.length === mintCountBefore + 1, "one proposal request minted"));
  const minted = mintBodies.at(-1)!;
  equal(
    Object.keys(minted).sort(),
    ["campaign_id", "message_id", "request_id"],
    "the mint body carries exactly the three ids",
  );
  const openCampaignId = openDoc().campaign_id as string;
  assert(minted.campaign_id === openCampaignId, "mint names the open campaign");
  assert(minted.message_id === sendBodies.at(-1)?.message, "mint names the scoped chat message");
  assert(/^req-[0-9a-f]{24}$/.test(minted.request_id), "request id is identifier-safe");
  await settle(() =>
    assert(
      text().includes(`stamped source r${openDoc().revision}`) && text().includes(`draft r${openDoc().revision}`),
      "the minted request's host stamp renders beside the draft revision",
    ),
  );
  await settle(() => assert(text().includes("Watching for the assistant's draft"), "the bounded watch starts"));

  // The assistant's live turn redeems the request (daemon socket —
  // this fixture's `redeem` plays the store-side part the browser
  // can never reach): the next poll lands the verified proposal.
  const mintedId = minted.request_id as string;
  redeem(mintedId);
  await settle(() => assert(text().includes("Verified assistant draft"), "the verified badge renders on the receipted proposal"), 30000);
  assert(text().includes("crm-writer"), "the badge names the receipt's agent");
  assert(text().includes(`request ${mintedId}`), "the badge names the receipt's request");
  assert(text().includes(`Verified assistant draft landed for request ${mintedId}`), "the poll stop note reports the match");

  // Apply the verified draft: expected_revision = current draft, the
  // revision moves, approval invalidates and the stale row clears.
  const revisionBefore = openDoc().revision as number;
  const verifiedLi = host.querySelector('[data-proposal^="prop-asst-"]');
  assert(verifiedLi, "the verified proposal row exists");
  await click(Array.from(verifiedLi!.querySelectorAll("button")).find((b) => (b.textContent ?? "").includes("Apply")));
  await settle(() => assert(text().includes("Applied as revision"), "verified apply reports the new revision"));
  assert(applyBodies.at(-1)?.expected_revision === revisionBefore, "verified apply sent the current revision");
  assert(text().includes("approval invalidated"), "apply invalidated approval visibly");
  await settle(() =>
    assert(
      !text().includes(`request ${mintedId}`) || !text().includes("Verified assistant draft"),
      "the decided proposal leaves the pending list",
    ),
  );


  // Test-send is a real one-recipient SMTP send — the fixture's
  // sender binding is live, so the button is armed and the receipt
  // records SMTP acceptance only.
  await fillInput("#cmp-test-email", "qa@example.com");
  await click(byText("button", "Send test"));
  await settle(() =>
    assert(
      text().includes("qa@example.com") && text().includes("not proof of inbox delivery"),
      "test receipt reports SMTP acceptance only",
    ),
  );

  // Open the full detail directly: the saved campaign renders in the
  // CAD-1008 task order — Content+Preview → approval → Audience/freeze
  // → Sender → Test → Proposals → Final send — and the saved revision
  // renders in the preview without a manual render click.
  await click(byText("button", "Open campaign detail →"));
  await settle(() => assert(host.querySelector('section[aria-label="Frozen audience"]'), "detail names its freeze panel"));
  await settle(() => {
    const frame = host.querySelector('iframe[data-preview="visual"]') as HTMLIFrameElement;
    assert(frame && (frame.getAttribute("srcdoc") ?? "").includes("HTML form"), "the saved campaign auto-renders its preview on open");
  });
  {
    const labels = [
      'div[aria-label="Email content"]',
      'section[aria-label="Email preview"]',
      'section[aria-label="Content approval"]',
      'section[aria-label="Frozen audience"]',
      'section[aria-label="SMTP sender"]',
      'section[aria-label="Test send"]',
      'section[aria-label="Assistant proposals"]',
      'section[aria-label="Final send"]',
    ];
    const seen: number[] = [];
    for (const sel of labels) {
      const el = host.querySelector(sel);
      assert(el, `panel renders: ${sel}`);
      const pos = Array.from(host.querySelectorAll("form, section")).indexOf(el as Element);
      seen.push(pos);
    }
    const ordered = seen.every((pos, i) => i === 0 || pos > seen[i - 1]);
    assert(ordered, `panels render in the saved-campaign task order: ${seen.join(",")}`);
  }
  await click(byText("button", "Recheck freeze"));
  await settle(() => assert(text().includes("Valid"), "freeze validity rechecks live"));

  // CAD-1008 stale-render guard: a slow saved-render answer that lands
  // after a newer save can never claim the new revision. Hold the next
  // render response, save revision N+1, then release the stale answer —
  // the preview must still render the newer saved revision, not the
  // stale bytes.
  const renderDoc = () => contents[openDoc().campaign_id];
  const renderResponse = (revision: number, html: string, text: string, digest: string) =>
    new Response(JSON.stringify({ render: {
      campaign_id: renderDoc().campaign_id, install_id: "install-crm", context_id: "ctx-a",
      revision, content_digest: renderDoc().content_digest, sample_first_name: null,
      binding: { binding_id: "preview", revision: 1, digest: "binding-digest", preview_only: true },
      preview_only: true, send_ready: false,
      sender: { name: "Cadence CRM", address: "noreply@cadence.invalid" },
      unsubscribe_url: "https://cadence.invalid/unsubscribe/preview",
      html, text, render_digest: digest,
    } }), { status: 200, headers: { "Content-Type": "application/json" } });
  let heldRender: ((value: Response) => void) | null = null;
  const innerRenderFetch = globalThis.fetch;
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = new URL(String(input), "http://localhost");
    if (init?.method === "POST" && url.pathname.endsWith("/render") && heldRender === null) {
      return new Promise<Response>((resolve) => {
        heldRender = resolve;
      });
    }
    return innerRenderFetch(input as RequestInfo | URL, init);
  }) as typeof fetch;
  // Refresh once: the held response answers with the current revision
  // while we save the next one.
  await click(byText("button", "Refresh preview"));
  await settle(() => assert(heldRender !== null, "the refresh render is in flight"));
  const heldRevision = renderDoc().revision;
  // Save a newer revision while the stale render is outstanding.
  await click(byText("button", "Edit subject / text"));
  await settle(() => assert(host.querySelector("#cmp-subject"), "inline edit opens for the stale-render save"));
  await fillInput("#cmp-subject", "Assistant draft subject v4");
  await click(byText("button", "Save text corrections (new revision)"));
  await settle(() => assert(renderDoc().revision === heldRevision + 1, "the newer revision saved while the stale render was in flight"));
  // Release the stale answer now that the saved revision has moved on.
  const release = heldRender!;
  heldRender = null;
  await React.act(async () => { release(renderResponse(heldRevision, "<h1>STALE render</h1>", "STALE", `stale-${heldRevision}`)); });
  await settle(() => {
    const frame = host.querySelector('iframe[data-preview="visual"]') as HTMLIFrameElement | null;
    assert(frame, "the preview re-rendered after the save");
    const src = frame.getAttribute("srcdoc") ?? "";
    assert(!src.includes("STALE"), "the stale render never claimed the new revision");
    assert(src.includes("HTML form"), "the preview shows the current saved revision");
  });
  globalThis.fetch = innerRenderFetch;

  // CAD-1008 receipt guard: a render answer whose revision/digest
  // differs from the saved doc it was requested for is never shown as
  // the current email — it surfaces a reload-needed mismatch instead.
  const currentRevision = renderDoc().revision;
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = new URL(String(input), "http://localhost");
    if (init?.method === "POST" && url.pathname.endsWith("/render")) {
      return renderResponse(currentRevision + 99, "<h1>FUTURE render</h1>", "FUTURE", `future-${currentRevision + 99}`);
    }
    return innerRenderFetch(input as RequestInfo | URL, init);
  }) as typeof fetch;
  await click(byText("button", "Refresh preview"));
  await settle(() => assert(text().includes("changed to r") || text().includes("reload the campaign"), "a mismatched render receipt demands a reload, not a false preview"));
  assert(!text().includes("FUTURE render"), "the mismatched receipt never renders as the current email");
  globalThis.fetch = innerRenderFetch;

  // Direct HTTP failures surface: an unknown campaign ID refuses.
  await React.act(async () => { root.unmount(); });
  history.pushState(null, "", "/app-installations/install-crm?ctx=ctx-a&crm=campaigns&record=campaign-missing");
  await React.act(async () => {
    const root2 = createRoot(host);
    root2.render(React.createElement(AppShell, { installId: "install-crm", viewer: { operator: true, readOnly: false } }));
    (globalThis as any).__cmpRoot2 = root2;
  });
  await settle(() => assert(text().includes("unavailable for this installation and context"), "unknown campaign refuses through the route"));
  await React.act(async () => { (globalThis as any).__cmpRoot2.unmount(); });

  // An unknown section never renders: the route strips back to Customers.
  history.pushState(null, "", "/app-installations/install-crm?ctx=ctx-a&crm=bogus");
  await React.act(async () => {
    const root3 = createRoot(host);
    root3.render(React.createElement(AppShell, { installId: "install-crm", viewer: { operator: true, readOnly: false } }));
    (globalThis as any).__cmpRoot3 = root3;
  });
  await settle(() => assert(!location.search.includes("crm="), "unknown section strips to the default route"));
  await settle(() => assert(host.querySelector('section[aria-label="Customers list"]'), "unknown section falls back to Customers"));
  await React.act(async () => { (globalThis as any).__cmpRoot3.unmount(); });

  console.log("crm campaigns checks passed");
}

void main();
