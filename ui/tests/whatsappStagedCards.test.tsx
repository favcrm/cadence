export {};
/**
 * CAD-2649 / TASK-013: Home approval cards and chat turn summaries for staged
 * WABA messaging and lifecycle operations (`app: "waba"`, `whatsapp.*`).
 */
import type { Overview } from "../src/lib/types";
import type { ThreadEntry } from "../src/features/home/thread";
import type { ResourceState } from "../src/lib/cache";

declare function require(name: string): any;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/" });
  for (const name of [
    "window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement",
    "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator",
    "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent",
    "location", "history", "sessionStorage",
  ]) {
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  }
  for (const name of ["addEventListener", "removeEventListener"]) {
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  }
  Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

  const loader = require("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, arguments);
  };

  const React = require("react") as typeof import("react");
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const NeedsRail = (require("../src/features/home/NeedsRail") as typeof import("../src/features/home/NeedsRail")).default;
  const { resetTodoLocal } = require("../src/features/home/todoLocal") as typeof import("../src/features/home/todoLocal");
  const { homeDecisions } = require("../src/features/decisions/useDecisions") as typeof import("../src/features/decisions/useDecisions");
  const {
    parseStagedRow,
    outcomeOf,
  } = require("../src/features/decisions/stagedWrite") as typeof import("../src/features/decisions/stagedWrite");
  const {
    StagedSourceContext,
  } = require("../src/ui/StagedChip") as typeof import("../src/ui/StagedChip");
  const {
    TurnStagedCards,
  } = require("../src/features/home/TurnRichCards") as typeof import("../src/features/home/TurnRichCards");
  const {
    stagedFields,
    stagedAccount,
    stagedStatus,
  } = require("../src/features/home/turnCards") as typeof import("../src/features/home/turnCards");

  const RLO = "\u202E";
  const ZWSP = "\u200B";
  const change = (field: string, label: string, after: unknown) => ({ field, label, kind: "added", after });
  const digest = "a".repeat(64);
  const sha = "b".repeat(64);

  const wabaRow = (
    id: string,
    operation: string,
    title: string,
    changes: ReturnType<typeof change>[],
    extra: Record<string, unknown> = {},
  ) => ({
    id,
    tool: "app_call",
    status: "pending",
    createdAt: Date.now() - 30_000,
    decidedAt: null,
    app: "WhatsApp Business",
    title,
    target: { kind: "whatsapp_conversation", label: "Alice +852 9000 0000", id: "conv_1" },
    input: {
      app: "waba",
      operation,
      args: JSON.stringify({ provider: changes.find((c) => c.field === "provider")?.after ?? "official" }),
    },
    origin: { via: "mcp", kind: "agent", label: "Support Agent" },
    changes,
    unchanged_count: 0,
    changes_complete: true,
    effects: ["Sends an external WhatsApp message."],
    result: null,
    ...extra,
  });

  // 1. Parse all messaging, handoff, and lifecycle operations + attachment + bidi neutralization + uncertain outcome.
  {
    const replyWithAttachment = parseStagedRow(wabaRow(
      "0123456789abcdef0123456789abcdef",
      "whatsapp.reply.stage",
      `Send ${RLO}WhatsApp reply`,
      [
        change("provider", "Provider", "linked"),
        change("conversation_id", "Conversation", "15550001111@s.whatsapp.net"),
        change("reply_to_message_id", "Reply to message", "msg_1"),
        change("text", "Message", `Hello ${ZWSP}Alice!`),
        change("digest", "Proposal digest", digest),
        change("attachment", "Attachment", "invoice.pdf (document, 1234 bytes)"),
        change("attachment_sha256", "Attachment SHA-256", sha),
      ],
      {
        target: { kind: "whatsapp_conversation", label: `Alice${RLO} Spoof`, id: "15550001111@s.whatsapp.net" },
      },
    ));
    assert(replyWithAttachment !== null && replyWithAttachment.whatsapp?.provider === "linked", "linked reply parses");
    assert(replyWithAttachment.whatsapp.providerLabel === "Linked Device", "linked provider label resolves");
    assert(replyWithAttachment.whatsapp.attachment?.sha256 === sha, "attachment SHA-256 extracted");
    assert(!replyWithAttachment.whatsapp.destructive, "single reply is non-destructive");
    assert(replyWithAttachment.title.includes("[U+202E]") && !replyWithAttachment.title.includes(RLO), "title bidi made visible");
    assert(replyWithAttachment.target?.label.includes("[U+202E]"), "target label bidi made visible");
    assert(stagedAccount(replyWithAttachment) === "Linked Device", "turn card account shows Linked Device");

    const fieldsInfo = stagedFields(replyWithAttachment);
    assert(fieldsInfo.body === "Hello [U+200B]Alice!", `reply text becomes clamped body (${fieldsInfo.body})`);
    assert(
      fieldsInfo.fields.some((f) => f.label === "Attachment" && f.value.includes("invoice.pdf")) &&
      fieldsInfo.fields.some((f) => f.label === "Attachment SHA-256" && f.value === sha),
      "attachment and SHA-256 stay prioritized ahead of digest in turn card fields",
    );

    const template = parseStagedRow(wabaRow(
      "1123456789abcdef0123456789abcdef",
      "whatsapp.template.stage",
      "Send Official WhatsApp template",
      [
        change("provider", "Provider", "official"),
        change("conversation_id", "Conversation", "conv_official_1"),
        change("template_id", "Template", "order_ready"),
        change("variables", "Variables", { "1": "Alice" }),
        change("digest", "Proposal digest", digest),
      ],
    ));
    assert(template?.whatsapp?.providerLabel === "Official API" && template.whatsapp.category === "message", "official template parses");
    assert(stagedAccount(template!) === "Official API", "turn card account shows Official API");

    for (const [op, destructive] of [
      ["whatsapp.broadcast.test.stage", false],
      ["whatsapp.broadcast.stage", true],
      ["whatsapp.broadcast.stop.stage", true],
      ["whatsapp.handoff.stage", false],
      ["whatsapp.connect", false],
      ["whatsapp.start", false],
      ["whatsapp.stop", true],
      ["whatsapp.disconnect", true],
      ["whatsapp.link", true],
      ["whatsapp.rescope", true],
    ] as const) {
      const parsed = parseStagedRow(wabaRow(
        "2123456789abcdef0123456789abcdef",
        op,
        `WhatsApp ${op}`,
        [change("provider", "Provider", op === "whatsapp.link" || op === "whatsapp.rescope" ? "official" : "linked")],
      ));
      assert(parsed?.whatsapp?.operation === op, `${op} parsed`);
      assert(parsed.whatsapp.destructive === destructive, `${op} destructive=${destructive}`);
      assert(!destructive || !!parsed.whatsapp.warning, `${op} carries warning text when destructive`);
    }

    const uncertainStop = parseStagedRow(wabaRow(
      "3123456789abcdef0123456789abcdef",
      "whatsapp.stop",
      "Stop WhatsApp",
      [change("provider", "Provider", "linked")],
      { status: "approved", decidedAt: Date.now(), result: { ok: false, code: "uncertain" } },
    ));
    assert(uncertainStop !== null && outcomeOf(uncertainStop) === "unconfirmed", "uncertain WhatsApp result is unconfirmed");
    assert(stagedStatus(uncertainStop, outcomeOf(uncertainStop), "Could not confirm") === "Could not confirm", "unconfirmed never reads as Applied or Sent");
  }

  // 2. Render in Home rail + StagedWriteDetail drawer and verify warning, provider mode, and bidi escaping.
  let actions: unknown[] = [
    wabaRow(
      "4123456789abcdef0123456789abcdef",
      "whatsapp.disconnect",
      "Disconnect Linked WhatsApp",
      [
        change("provider", "Provider", "linked"),
        change("impact", "Impact", `Wipes credentials ${RLO}now`),
        change("digest", "Proposal digest", digest),
      ],
      { target: { kind: "whatsapp_connection", label: "Linked Device", id: "linked" } },
    ),
    wabaRow(
      "5123456789abcdef0123456789abcdef",
      "whatsapp.broadcast.stage",
      "Send Official Broadcast",
      [
        change("provider", "Provider", "official"),
        change("broadcast_id", "Broadcast", "bc_42"),
        change("to", "Recipient", "All VIP"),
        change("digest", "Proposal digest", digest),
      ],
      { target: { kind: "whatsapp_broadcast", label: "VIP Promo", id: "bc_42" } },
    ),
  ];

  const jsonResp = (v: unknown) => new Response(JSON.stringify(v), { status: 200, headers: { "Content-Type": "application/json" } });
  globalThis.fetch = (async (input: unknown) => {
    const url = new URL(String(input), "http://localhost");
    if (url.pathname === "/api/assist/actions") return jsonResp({ actions });
    return jsonResp({});
  }) as typeof fetch;

  const flush = () => React.act(async () => { await new Promise((r) => setTimeout(r, 0)); });
  const tick = () => React.act(async () => { await new Promise((r) => setTimeout(r, 15)); });
  async function settle(check: () => void) {
    for (let i = 0; i < 60; i++) {
      await flush();
      await tick();
      try { check(); return; } catch { /* retry */ }
    }
    await flush();
    check();
  }

  resetTodoLocal();
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const overview = { status: "ok", inFlight: false, error: null, asOf: Date.now(), data: { needs_me: [] } } as unknown as ResourceState<Overview>;
  await React.act(async () => {
    root.render(React.createElement(NeedsRail, {
      overview,
      readOnly: false,
      onOpenIssue: () => {},
      permissionsHref: "/settings/permissions",
      onAsk: () => {},
      onOpenDecision: () => {},
      onAskAgent: () => {},
      decisions: true,
    }));
  });

  await settle(() => assert(host.querySelectorAll('[data-need="staged_write"]').length === 2, "both WhatsApp staged cards appear"));
  const firstHit = host.querySelector('[data-need="staged_write"] .todo-hit');
  assert(firstHit, "hit button exists");
  await React.act(async () => {
    firstHit!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await settle(() => assert(document.querySelector('.crm-drawer[data-drawer="staged"]'), "drawer opens"));
  const drawer = document.querySelector('.crm-drawer[data-drawer="staged"]')!;
  const drawerText = drawer.textContent ?? "";
  assert(drawerText.includes("Linked Device"), "drawer displays normalized Linked Device provider");
  assert(drawer.querySelector('[data-whatsapp-warning="whatsapp.disconnect"]'), "destructive disconnect warning renders");
  assert(drawerText.includes("[U+202E]") && !drawerText.includes(RLO), "bidi neutralized inside rendered drawer");
  assert(!/"qr"\s*:/.test(drawerText), "never renders any QR secret");

  // 3. TurnStagedCards renders TurnCard with destructive warning & provider label.
  const turnRow = parseStagedRow(actions[1]);
  assert(turnRow !== null, "broadcast row parses");
  const entries: ThreadEntry[] = [
    {
      seq: 1,
      role: "agent",
      kind: "tool_result",
      text: JSON.stringify({ staged: true, action_id: "5123456789abcdef0123456789abcdef" }),
    },
  ];
  const turnHost = document.createElement("div");
  document.body.append(turnHost);
  const turnRoot = createRoot(turnHost);
  const snapshot = {
    phase: "ready" as const,
    rows: [turnRow],
    pending: [turnRow],
    decided: [],
    attempts: new Map(),
    notice: null,
    failedUnseen: false,
  };
  await React.act(async () => {
    turnRoot.render(
      React.createElement(
        StagedSourceContext.Provider,
        {
          value: {
            subscribe: () => () => {},
            currentDecisions: () => snapshot,
            requestReview: () => {},
          },
        },
        React.createElement(TurnStagedCards, { toolEntries: entries, density: "full" }),
      ),
    );
  });
  const card = turnHost.querySelector('[data-kind="staged-action"]');
  assert(card !== null, "turn staged card rendered");
  assert(card.textContent?.includes("Official API"), "turn card displays Official API provider");
  assert(card.querySelector('[data-whatsapp-warning="whatsapp.broadcast.stage"]'), "turn card displays broadcast warning");

  await React.act(async () => {
    turnRoot.unmount();
    root.unmount();
  });
  turnHost.remove();
  host.remove();
  homeDecisions.resetDecisions();
  console.log("whatsapp staged cards checks passed");
}

void main();
