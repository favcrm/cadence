/**
 * CAD-1946 Hosted merge gate UI tests:
 * - All 8 ApproveCardState states + 4-stage stepper + checklist + SafeLink proofs
 * - Proactive OTP step-up (`step_up_fresh: false`) and reactive `403 step_up_required`
 * - Idempotent `request_id` retry after OTP verification
 * - Stop merge (`stopped` and `already_merged`), Send back, `409 head_moved`
 * - Member refusal (`403 member_role`) and PrPanel hosted card mounting
 * - Clean fallback to LegacyMergeReview on `501` / generic `404` / `{}`
 */
import type { ApproveCardState, ApproveCardView } from "../src/lib/api";
import type { IssueDetail } from "../src/lib/types";
import type { HomeNeed } from "../src/features/home/needs";

declare function require(name: string): any;
declare const process: { exit(code?: number): never };

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

const HEAD1 = "1".repeat(40);
const HEAD2 = "2".repeat(40);
const BASE_SHA = "0".repeat(40);

function makeCard(
  state: ApproveCardState,
  overrides: Partial<ApproveCardView> = {},
): ApproveCardView {
  const stageMap: Record<ApproveCardState, ApproveCardView["progress_stage"]> = {
    needs_1: null,
    needs_n: null,
    ready: "checks",
    merging: "in_queue",
    merged: "merged",
    head_moved: "checks",
    ejected: "approved",
    unavailable: null,
  };
  return {
    ok: true,
    project: "demo",
    issue: "CAD-1946",
    repo: "favcrm/cadence",
    pr: 42,
    title: "Hosted merge gate rollout",
    pr_url: "https://github.com/favcrm/cadence/pull/42",
    state,
    headline: `Headline for ${state}`,
    progress_stage: overrides.progress_stage !== undefined ? overrides.progress_stage : stageMap[state],
    can_approve_and_merge: state === "ready" || state === "ejected",
    can_revoke: state === "merging",
    can_send_back: state !== "merged",
    step_up_fresh: true,
    checklist: [
      { id: "ci", label: "Required checks", status: state === "ready" || state === "merging" || state === "merged" ? "pass" : "pending", summary: "delivery-gate green" },
      { id: "review", label: "Independent review", status: state === "needs_1" || state === "needs_n" ? "fail" : "pass", summary: "1 reviewer recorded" },
    ],
    details: {
      head_sha: HEAD1,
      base_ref: "main",
      base_sha: BASE_SHA,
      merge_base_sha: BASE_SHA,
      delivery_class: "human",
      policy_digest: "sha256:abc123",
      approval_kind: state === "merging" || state === "merged" ? "hosted_board_otp" : null,
      approval_id: state === "merging" || state === "merged" ? "app_42" : null,
      evidence_id: state === "merging" || state === "merged" ? "ev_42" : null,
      reasons: state === "ejected" ? ["Merge queue rejected this head due to conflict."] : [],
      check_proofs: [
        {
          context: "delivery-gate",
          workflow_path: ".github/workflows/delivery-gate.yml",
          run_url: "https://github.com/favcrm/cadence/actions/runs/12345",
        },
      ],
      counted_verdicts: [
        {
          kind: "standards",
          outcome: "PASS",
          principal: "reviewer-bot",
          github_login: "reviewer-bot",
          head_sha: HEAD1,
        },
      ],
    },
    ...overrides,
  };
}

function makeNeed(): HomeNeed {
  return {
    key: "merge:CAD-1946",
    kind: "merge_decision",
    label: "merge",
    title: "Hosted merge gate rollout",
    shortTitle: "Hosted merge gate rollout",
    why: "Awaits owner approval",
    owner: "operator",
    age: 90,
    project: "demo",
    subject: { kind: "issue", id: "CAD-1946" },
    summary: null,
    escalatedBy: null,
    command: "cadence audit approve --issue CAD-1946",
    link: "https://github.com/favcrm/cadence/pull/42",
    issue: "CAD-1946",
    agent: null,
    action: {
      type: "merge",
      issue: "CAD-1946",
      pr: "favcrm/cadence#42",
      sha: HEAD1,
      reviewer: "reviewer-bot",
      verdict: "PASS",
    },
  };
}

function makeIssueDetail(): IssueDetail {
  return {
    id: "CAD-1946",
    project: "demo",
    title: "Hosted merge gate rollout",
    status: "review",
    status_source: "direct",
    priority: "P1",
    owner: "operator",
    blocked_by: [],
    relates: [],
    refs: [{ kind: "pr", label: "favcrm/cadence#42", url: "https://github.com/favcrm/cadence/pull/42" }],
    paths: [],
    tags: [],
    container: false,
    ready: true,
    blocked: false,
    created: "2026-11-15T00:00:00Z",
    rev: "fnv1a:0123456789abcdef",
    checks: { done: 1, total: 1 },
    work: { type: "task", type_source: "implicit", weight: 1 },
    frontmatter: {},
    body: "## Summary\n\nHosted gate test.",
    path: "issue.md",
    links: { children: [], blocked_by: [], blocks: [], relates: [], duplicates: [] },
    comments: [],
    notes_chain: [],
    artifacts: [],
    activity: [],
    commits: [],
    commits_skipped: [],
    reports: [],
  };
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/" });
  for (const name of [
    "window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement",
    "HTMLTextAreaElement", "HTMLButtonElement", "SVGElement", "navigator",
    "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent",
    "location", "history", "HTMLAnchorElement",
  ]) {
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  }
  for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"]) {
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  }
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true, configurable: true });

  const loader = require("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, arguments);
  };

  let nextGateResponse: () => { status: number; body: unknown } = () => ({ status: 200, body: makeCard("ready") });
  let nextMergeResponse: (body: Record<string, unknown>) => { status: number; body: unknown } = () => ({
    status: 200,
    body: {
      ok: true,
      state: "merging",
      approval_id: "app_42",
      evidence_id: "ev_42",
      recorded_via: "hosted_board",
      board_revocable: true,
      queued: true,
      auto_merge_enabled: true,
      comment_url: null,
    },
  });
  let nextRevokeResponse: () => { status: number; body: unknown } = () => ({
    status: 200,
    body: { ok: true, state: "revoked", approval_id: "app_42", outcome: "stopped" },
  });
  let nextSendBackResponse: () => { status: number; body: unknown } = () => ({
    status: 200,
    body: { ok: true, state: "sent_back", issue: "CAD-1946", ticket: {}, outcome: "stopped" },
  });
  let stepUpVerifyStatus = 200;

  const requests: { method: string; path: string; body?: Record<string, unknown> }[] = [];

  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const rawUrl = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    const url = new URL(rawUrl, "http://localhost");
    const method = (init?.method ?? "GET").toUpperCase();
    const parsedBody =
      typeof init?.body === "string" && init.body.trim()
        ? (JSON.parse(init.body) as Record<string, unknown>)
        : undefined;
    requests.push({ method, path: url.pathname, body: parsedBody });

    const reply = (status: number, body: unknown) =>
      new Response(JSON.stringify(body), {
        status,
        headers: { "Content-Type": "application/json" },
      });

    if (url.pathname === "/api/approvals/gate") {
      const r = nextGateResponse();
      return reply(r.status, r.body);
    }
    if (url.pathname === "/api/approvals/step-up/send") {
      return reply(200, { challenge_id: "chal_1", expires_at: Date.now() + 600_000, email_hint: "o***@fav.hk" });
    }
    if (url.pathname === "/api/approvals/step-up/verify") {
      if (stepUpVerifyStatus !== 200) {
        return reply(stepUpVerifyStatus, { error: { code: "invalid_code", message: "Invalid verification code." } });
      }
      return reply(200, { ok: true, authenticated_at: Date.now(), fresh_until: Date.now() + 900_000 });
    }
    if (url.pathname === "/api/approvals/merge") {
      const r = nextMergeResponse(parsedBody ?? {});
      return reply(r.status, r.body);
    }
    if (url.pathname.startsWith("/api/approvals/") && url.pathname.endsWith("/revoke")) {
      const r = nextRevokeResponse();
      return reply(r.status, r.body);
    }
    if (url.pathname === "/api/approvals/send-back") {
      const r = nextSendBackResponse();
      return reply(r.status, r.body);
    }
    if (url.pathname.startsWith("/api/issues/")) {
      return reply(200, makeIssueDetail());
    }
    if (url.pathname === "/api/overview") {
      return reply(200, { needs_me: [] });
    }
    return reply(200, {});
  }) as typeof fetch;

  const React = require("react") as typeof import("react");
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
  const DecisionPanel = (require("../src/features/home/DecisionPanel") as typeof import("../src/features/home/DecisionPanel")).default;
  const { PrPanel } = require("../src/features/issues/PrPanel") as typeof import("../src/features/issues/PrPanel");

  const flush = async () => {
    await React.act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
  };
  const tick = () =>
    React.act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });
  async function settle(check: () => void) {
    for (let i = 0; i < 60; i++) {
      await flush();
      await tick();
      try {
        check();
        return;
      } catch {
        // retry
      }
    }
    await flush();
    check();
  }
  async function click(el: Element | null | undefined) {
    assert(el, "button exists to click");
    await React.act(async () => {
      el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flush();
  }
  async function setInput(el: HTMLInputElement | HTMLTextAreaElement | null, val: string) {
    assert(el, "input exists");
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    const setter = Object.getOwnPropertyDescriptor(proto, "value")?.set;
    await React.act(async () => {
      setter?.call(el, val);
      el.dispatchEvent(new Event("input", { bubbles: true }));
      el.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await flush();
  }
  const btnAll = (root: ParentNode, text: string) =>
    Array.from(root.querySelectorAll("button")).filter((b) => b.textContent?.trim() === text) as HTMLButtonElement[];
  const btn = (root: ParentNode, text: string) => btnAll(root, text)[0];

  async function mountDecisionPanel(hosted = true, readOnly = false) {
    const doneCalls: { key: string; text: string }[] = [];
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    await React.act(async () => {
      root.render(
        React.createElement(
          LocaleProvider,
          null,
          React.createElement(DecisionPanel, {
            need: makeNeed(),
            readOnly,
            hosted,
            index: 1,
            total: 1,
            onDone: (key, text) => doneCalls.push({ key, text }),
            onClose: () => {},
            onAsk: () => {},
          }),
        ),
      );
    });
    await flush();
    return {
      host,
      doneCalls,
      unmount: async () => {
        await React.act(async () => {
          root.unmount();
        });
        host.remove();
      },
    };
  }

  // 1. All 8 ApproveCardState states render cleanly with stepper, checklist, and SafeLink details.
  const allStates: ApproveCardState[] = [
    "needs_1",
    "needs_n",
    "ready",
    "merging",
    "merged",
    "head_moved",
    "ejected",
    "unavailable",
  ];
  for (const state of allStates) {
    requests.length = 0;
    nextGateResponse = () => ({ status: 200, body: makeCard(state) });
    const view = await mountDecisionPanel(true, false);
    await settle(() => {
      const gate = view.host.querySelector(`[data-gate-state="${state}"]`);
      assert(gate, `renders data-gate-state="${state}"`);
    });
    const text = view.host.textContent ?? "";
    assert(text.includes(`Headline for ${state}`), `shows headline for ${state}`);
    assert(view.host.querySelectorAll(".rv-gate-step").length === 4, `renders 4-stage stepper for ${state}`);
    assert(view.host.querySelectorAll(".rv-gate-check").length === 2, `renders checklist items for ${state}`);
    const detailsBtn = btn(view.host, "Details");
    await click(detailsBtn);
    assert(
      Boolean(view.host.querySelector('a[href="https://github.com/favcrm/cadence/actions/runs/12345"]')),
      `renders SafeLink check proof for ${state}`,
    );
    await view.unmount();
  }

  // 2. Proactive OTP step-up (`step_up_fresh: false`) -> send 6-digit code -> enter digits -> verify & merge -> transitions to merging.
  requests.length = 0;
  let mergeCount = 0;
  let currentCard = makeCard("ready", { step_up_fresh: false });
  nextGateResponse = () => ({ status: 200, body: currentCard });
  nextMergeResponse = () => {
    mergeCount++;
    currentCard = makeCard("merging");
    return {
      status: 200,
      body: {
        ok: true, state: "merging", approval_id: "app_42", evidence_id: "ev_42",
        recorded_via: "hosted_board", board_revocable: true, queued: true, auto_merge_enabled: true, comment_url: null,
      },
    };
  };
  {
    const view = await mountDecisionPanel(true, false);
    await settle(() => assert(btn(view.host, "Send 6-digit code"), "proactive OTP shows Send 6-digit code"));
    assert(btnAll(view.host, "Send 6-digit code").length === 1, "does not duplicate Send 6-digit code button");
    assert(!btn(view.host, "Approve & merge"), "does not show conflicting Approve & merge before code is sent");
    await click(btn(view.host, "Send 6-digit code"));
    await settle(() => assert((view.host.textContent ?? "").includes("o***@fav.hk"), "shows email hint after send"));
    const otpInput = view.host.querySelector('input[inputmode="numeric"]') as HTMLInputElement | null;
    await setInput(otpInput, "12a34b56");
    assert(otpInput?.value === "123456", "strips non-digits and clamps to 6 digits");
    await click(btn(view.host, "Verify & merge"));
    await settle(() => assert(view.host.querySelector('[data-gate-state="merging"]'), "transitions to merging after verify & merge"));
    assert(mergeCount === 1, "merge dispatched once");
    assert(Boolean(btn(view.host, "Done")), "merging state promotes Done primary action");
    assert(Boolean(btn(view.host, "Stop merge")), "merging state offers Stop merge");
    await view.unmount();
  }

  // 3. Reactive 403 `step_up_required` + idempotent request_id preserved on retry after OTP verify.
  requests.length = 0;
  let attempt = 0;
  const seenRequestIds: string[] = [];
  currentCard = makeCard("ready", { step_up_fresh: true });
  nextGateResponse = () => ({ status: 200, body: currentCard });
  nextMergeResponse = (body) => {
    attempt++;
    if (typeof body.request_id === "string") seenRequestIds.push(body.request_id);
    if (attempt === 1) {
      return { status: 403, body: { error: { code: "step_up_required", message: "Recent OTP verification is required." } } };
    }
    currentCard = makeCard("merging");
    return {
      status: 200,
      body: {
        ok: true, state: "merging", approval_id: "app_42", evidence_id: "ev_42",
        recorded_via: "hosted_board", board_revocable: true, queued: true, auto_merge_enabled: true, comment_url: null,
      },
    };
  };
  {
    const view = await mountDecisionPanel(true, false);
    await settle(() => assert(btn(view.host, "Approve & merge"), "renders Approve & merge"));
    await click(btn(view.host, "Approve & merge"));
    await settle(() => assert(view.host.querySelector('input[inputmode="numeric"]'), "403 step_up_required auto-sends challenge and opens OTP input"));
    await setInput(view.host.querySelector('input[inputmode="numeric"]'), "654321");
    await click(btn(view.host, "Verify & merge"));
    await settle(() => assert(view.host.querySelector('[data-gate-state="merging"]'), "retries merge cleanly after verify"));
    assert(seenRequestIds.length === 2 && seenRequestIds[0] === seenRequestIds[1], `preserves idempotent request_id across OTP retry (${JSON.stringify(seenRequestIds)})`);
    await view.unmount();
  }

  // 4. Stop merge covers both `stopped` and `already_merged`, plus `409 head_moved` refresh and Send back.
  requests.length = 0;
  let gateHead = HEAD1;
  nextGateResponse = () => ({ status: 200, body: makeCard("merging", { details: { ...makeCard("merging").details, head_sha: gateHead } }) });
  nextRevokeResponse = () => ({ status: 200, body: { ok: true, state: "revoked", approval_id: "app_42", outcome: "already_merged" } });
  {
    const view = await mountDecisionPanel(true, false);
    await settle(() => assert(btn(view.host, "Stop merge"), "shows Stop merge"));
    await click(btn(view.host, "Stop merge"));
    const box = view.host.querySelector(".rv-sendback textarea") as HTMLTextAreaElement | null;
    await setInput(box, "Wait for final check");
    await click(btn(view.host, "Confirm stop merge"));
    await settle(() =>
      assert(
        (view.host.textContent ?? "").includes("Pull request already merged before revocation could dequeue it."),
        "surfaces already_merged revoke outcome",
      ),
    );
    await view.unmount();
  }

  // 409 head_moved on merge refreshes gate card to new SHA
  requests.length = 0;
  let movedGateFetches = 0;
  nextGateResponse = () => {
    movedGateFetches++;
    return {
      status: 200,
      body: movedGateFetches === 1
        ? makeCard("ready")
        : makeCard("head_moved", { details: { ...makeCard("head_moved").details, head_sha: HEAD2 } }),
    };
  };
  nextMergeResponse = () => ({ status: 409, body: { error: { code: "head_moved", message: "PR head moved." } } });
  {
    const view = await mountDecisionPanel(true, false);
    await settle(() => assert(btn(view.host, "Approve & merge"), "ready for merge before head moves"));
    await click(btn(view.host, "Approve & merge"));
    await settle(() => assert(view.host.querySelector('[data-gate-state="head_moved"]'), "409 head_moved triggers gate refresh to head_moved"));
    assert((view.host.textContent ?? "").includes("Pull request updated — a new commit was pushed."), "renders moved-head warning");
    assert(Boolean(btn(view.host, "Review updated PR")), "head_moved primary button reads Review updated PR");
    await view.unmount();
  }

  // Send back from drawer calls /api/approvals/send-back and completes need.
  requests.length = 0;
  nextGateResponse = () => ({ status: 200, body: makeCard("ready") });
  nextSendBackResponse = () => ({ status: 200, body: { ok: true, state: "sent_back", issue: "CAD-1946", ticket: {}, outcome: "stopped" } });
  {
    const view = await mountDecisionPanel(true, false);
    await settle(() => assert(btn(view.host, "Send back…"), "Send back button visible"));
    await click(btn(view.host, "Send back…"));
    await setInput(view.host.querySelector(".rv-sendback textarea") as HTMLTextAreaElement | null, "Split PR first");
    await click(btn(view.host, "Send back"));
    await settle(() => assert(view.doneCalls.some((c) => c.text === "Sent back"), "Send back triggers onDone Sent back"));
    const sbReq = requests.find((r) => r.path === "/api/approvals/send-back");
    assert(sbReq?.body?.reason === "Split PR first" && sbReq.body.project === "demo", "send-back payload carries reason & project");
    await view.unmount();
  }

  // 5. Member refusal (`403 member_role`) shows owner-only guidance; PrPanel renders hosted card when supported.
  nextGateResponse = () => ({ status: 403, body: { error: { code: "member_role", message: "Owner required." } } });
  {
    const view = await mountDecisionPanel(true, false);
    await settle(() => assert((view.host.textContent ?? "").includes("Only the board's owner can approve or publish."), "member sees owner-only message"));
    await view.unmount();
  }

  nextGateResponse = () => ({ status: 200, body: makeCard("needs_n") });
  {
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    await React.act(async () => {
      root.render(
        React.createElement(
          LocaleProvider,
          null,
          React.createElement(PrPanel, {
            detail: makeIssueDetail(),
            readOnly: false,
            onWrite: (async () => {}) as never,
            onError: () => {},
          }),
        ),
      );
    });
    await settle(() => assert(host.querySelector('[data-gate-state="needs_n"]'), "PrPanel mounts live HostedPrGateSection when board serves /api/approvals/gate"));
    await React.act(async () => {
      root.unmount();
    });
    host.remove();
  }

  // 6. Clean fallback to LegacyMergeReview on HTTP 501, generic/route-missing 404, and mock `{}`.
  for (const fallback of [
    () => ({ status: 501, body: { error: { code: "not_implemented", message: "Local daemon" } } }),
    () => ({ status: 404, body: { error: { message: "Not found" } } }),
    () => ({ status: 404, body: { error: { code: "unknown_route", message: "Unknown route" } } }),
    () => ({ status: 200, body: {} }),
  ]) {
    nextGateResponse = fallback;
    const view = await mountDecisionPanel(true, false);
    await settle(() => assert(view.host.querySelector('[data-review-verb="Approve change"]'), "falls back to LegacyMergeReview when /api/approvals/gate is unsupported"));
    await view.unmount();
  }

  // 7. Domain 404s stay in HostedMergeGate; 403 non-role error mentioning "owner" does not trigger memberRestricted;
  //    403 invalid_code + 409 gate_refused map to clear user copy; Enter key submits OTP.
  for (const domain404 of [
    { code: "pr_not_found", expected: "Pull request was not found in the bound repository." },
    { code: "repo_not_bound", expected: "Repository is not bound to this hosted workspace." },
  ]) {
    nextGateResponse = () => ({ status: 404, body: { error: { code: domain404.code, message: domain404.code } } });
    const view = await mountDecisionPanel(true, false);
    await settle(() => assert((view.host.textContent ?? "").includes(domain404.expected), `stays on hosted gate for ${domain404.code}`));
    assert(!view.host.querySelector('[data-review-verb="Approve change"]'), `does not fall back for domain 404 ${domain404.code}`);
    assert(Boolean(btn(view.host, "Refresh gate")), `offers Refresh gate on domain 404 ${domain404.code}`);
    await view.unmount();
  }

  nextGateResponse = () => ({ status: 403, body: { error: { code: "email_not_configured", message: "Owner email not configured." } } });
  {
    const view = await mountDecisionPanel(true, false);
    await settle(() => assert((view.host.textContent ?? "").includes("Owner verification email is not configured for this workspace."), "renders email_not_configured refusal"));
    assert(!(view.host.textContent ?? "").includes("Only the board's owner can approve or publish."), "does not misclassify non-role 403 mentioning 'owner' as member_role");
    await view.unmount();
  }

  stepUpVerifyStatus = 403;
  nextGateResponse = () => ({ status: 200, body: makeCard("ready", { step_up_fresh: false }) });
  {
    const view = await mountDecisionPanel(true, false);
    await settle(() => assert(btn(view.host, "Send 6-digit code"), "renders Send 6-digit code"));
    await click(btn(view.host, "Send 6-digit code"));
    await settle(() => assert(view.host.querySelector('input[inputmode="numeric"]'), "renders OTP input"));
    const otpInput = view.host.querySelector('input[inputmode="numeric"]') as HTMLInputElement | null;
    await setInput(otpInput, "000000");
    await React.act(async () => {
      otpInput?.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    });
    await settle(() => assert((view.host.textContent ?? "").includes("That 6-digit code did not match. Check the email and try again."), "Enter key submits OTP and invalid_code maps clearly"));
    await view.unmount();
  }

  stepUpVerifyStatus = 200;
  nextGateResponse = () => ({ status: 200, body: makeCard("ready", { step_up_fresh: true }) });
  nextMergeResponse = () => ({ status: 409, body: { error: { code: "gate_refused", message: "checks failing" } } });
  {
    const view = await mountDecisionPanel(true, false);
    await settle(() => assert(btn(view.host, "Approve & merge"), "renders Approve & merge for 409 test"));
    await click(btn(view.host, "Approve & merge"));
    await settle(() => assert((view.host.textContent ?? "").includes("Merge gate refused this pull request. Review the checks above and refresh the gate."), "maps 409 gate_refused cleanly"));
    await view.unmount();
  }

  console.log("hostedMergeGate: all hosted merge gate UI assertions passed");
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
