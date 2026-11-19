export {};
/**
 * CAD-2476: Agent Inbox & assistant-to-assistant relay inside the Home rail.
 *
 *  - Inbox tab appears beside To do / Updates with live unread badge;
 *  - Filters (All / Colleagues / Specialists / Unread) narrow the feed;
 *  - Mark read sends POST /api/inbox/ack and updates unread count;
 *  - Opt-in Create issue sends POST /api/inbox/promote and renders a clickable issue link;
 *  - Message composer sends POST /api/inbox/send without creating a board ticket.
 */
import type { AgentInboxFeed, Overview } from "../src/lib/types";
import type { ResourceState } from "../src/lib/cache";

declare function require(name: string): any;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/" });
  for (const name of [
    "window",
    "document",
    "Node",
    "Element",
    "HTMLElement",
    "HTMLInputElement",
    "HTMLSelectElement",
    "HTMLTextAreaElement",
    "SVGElement",
    "navigator",
    "MutationObserver",
    "ResizeObserver",
    "Event",
    "MouseEvent",
    "KeyboardEvent",
    "location",
    "history",
    "sessionStorage",
  ]) {
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  }
  for (const name of ["addEventListener", "removeEventListener"]) {
    Object.defineProperty(globalThis, name, {
      value: win[name].bind(win),
      configurable: true,
    });
  }
  Object.defineProperty(globalThis, "crypto", {
    value: require("crypto").webcrypto,
    configurable: true,
  });
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
  const { act } = require("react") as typeof import("react");
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const NeedsRail = (require("../src/features/home/NeedsRail") as typeof import("../src/features/home/NeedsRail")).default;
  const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");

  const now = Math.floor(Date.now() / 1000);
  let state: AgentInboxFeed = {
    alias: "master",
    cursor: 0,
    tail_seq: 2,
    unread: 2,
    reader: "default",
    messages: [
      {
        seq: 2,
        id: "msg-fin-2",
        alias: "master",
        kind: "checkin",
        sender: "finance-agent",
        sender_uid: null,
        sender_kind: "specialist",
        recipient: "master",
        reply_to: "finance-agent",
        origin: null,
        body: "Invoice #4821 drafted; awaiting approval before posting.",
        artifact: null,
        state: "done",
        unread: true,
        auto_answered: false,
        issue: null,
        task_id: null,
        created: now - 60,
      },
      {
        seq: 1,
        id: "msg-maya-1",
        alias: "master",
        kind: "handoff",
        sender: "assist-maya",
        sender_uid: null,
        sender_kind: "colleague",
        recipient: "master",
        reply_to: "assist-maya",
        origin: null,
        body: "Q3 forecast deck ready for review.",
        artifact: "docs/q3-forecast.md",
        state: "done",
        unread: true,
        auto_answered: false,
        issue: null,
        task_id: null,
        created: now - 120,
      },
    ],
    directory: {
      colleagues: [
        {
          alias: "assist-maya",
          role: "assistant",
          provider: "fake",
          endpoint_kind: "managed",
          state: "idle",
          kind: "colleague",
        },
      ],
      specialists: [
        {
          alias: "finance-agent",
          role: "finance",
          provider: "fake",
          endpoint_kind: "managed",
          state: "idle",
          kind: "specialist",
        },
      ],
    },
  };

  const calls: Array<{ url: string; method: string; body?: unknown }> = [];
  const origFetch = globalThis.fetch;
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input.toString();
    const method = init?.method ?? "GET";
    const body = typeof init?.body === "string" ? JSON.parse(init.body) : undefined;
    calls.push({ url, method, body });

    const okJson = (payload: unknown) =>
      new Response(JSON.stringify(payload), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });

    if (url.startsWith("/api/inbox?")) return okJson(state);
    if (url === "/api/inbox/ack" && method === "POST") {
      const through = Number((body as { through?: number }).through ?? 0);
      state = {
        ...state,
        cursor: Math.max(state.cursor, through),
        messages: state.messages.map((m) => (m.seq <= through ? { ...m, unread: false } : m)),
      };
      state.unread = state.messages.filter((m) => m.unread).length;
      return okJson({ alias: "master", through, unread: state.unread });
    }
    if (url === "/api/inbox/promote" && method === "POST") {
      const msgId = String((body as { message?: string }).message ?? "");
      state = {
        ...state,
        messages: state.messages.map((m) => (m.id === msgId ? { ...m, issue: "CAD-901" } : m)),
      };
      return okJson({ ok: true, alias: "master", message: msgId, issue: "CAD-901" });
    }
    if (url === "/api/inbox/send" && method === "POST") {
      return okJson({ ok: true, id: "msg-sent-3", seq: 3 });
    }
    return new Response(JSON.stringify({ error: "not found" }), { status: 404 });
  }) as typeof fetch;

  const overview: ResourceState<Overview> = {
    data: {
      projects: [],
      needs_me: [],
      attention: [],
      active: [],
      counts: { todo: 0, in_progress: 0, in_review: 0, blocked: 0, done: 0 },
      recent_done: [],
      generated_at: now,
    } as unknown as Overview,
    status: "ok",
    error: null,
    asOf: Date.now(),
    inFlight: false,
  };

  const openedIssues: string[] = [];
  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);

  try {
    await act(async () => {
      root.render(
        React.createElement(
          LocaleProvider,
          null,
          React.createElement(NeedsRail, {
            overview,
            readOnly: false,
            onOpenIssue: (id: string) => openedIssues.push(id),
            overviewHref: "/overview",
            permissionsHref: "/settings",
            onAsk: () => {},
            onOpenDecision: () => {},
            collapsed: false,
            onToggleCollapse: () => {},
            onAskAgent: () => {},
          }),
        ),
      );
      await new Promise((r) => setTimeout(r, 20));
    });

    const inboxTab = Array.from(host.querySelectorAll(".todo-tabs button")).find((b) =>
      b.textContent?.includes("Inbox"),
    ) as HTMLButtonElement | undefined;
    assert(inboxTab, "Inbox tab should render in Home rail");
    assert(inboxTab.textContent?.includes("2"), `Expected unread badge 2, got ${inboxTab.textContent}`);

    // Open Inbox tab
    await act(async () => {
      inboxTab.click();
      await new Promise((r) => setTimeout(r, 20));
    });

    assert(host.querySelector("[data-agent-inbox]"), "InboxPanel should mount when Inbox tab is selected");
    assert(host.textContent?.includes("assist-maya"), "Colleague sender should render");
    assert(host.textContent?.includes("docs/q3-forecast.md"), "Handoff artifact chip should render");
    assert(host.textContent?.includes("finance-agent"), "Specialist sender should render");
    assert(host.textContent?.includes("no ticket"), "Unpromoted messages should indicate no ticket");

    // Filter to Colleagues only
    const colleaguesBtn = Array.from(host.querySelectorAll("[data-agent-inbox] button")).find(
      (b) => b.textContent?.trim() === "Colleagues",
    ) as HTMLButtonElement;
    await act(async () => {
      colleaguesBtn.click();
    });
    const listItems = host.querySelectorAll("[data-inbox-list] > li");
    assert(listItems.length === 1, `Colleagues filter should show 1 item, got ${listItems.length}`);
    assert(listItems[0].textContent?.includes("assist-maya"), "Filtered item should be assist-maya");

    // Expand assist-maya handoff, Mark read, and Create issue
    const expandBtn = listItems[0].querySelector("button") as HTMLButtonElement;
    await act(async () => {
      expandBtn.click();
    });

    const markReadBtn = Array.from(listItems[0].querySelectorAll("button")).find(
      (b) => b.textContent?.trim() === "Mark read",
    ) as HTMLButtonElement;
    assert(markReadBtn, "Mark read button should render for unread message");
    await act(async () => {
      markReadBtn.click();
      await new Promise((r) => setTimeout(r, 20));
    });
    assert(
      calls.some((c) => c.url === "/api/inbox/ack" && (c.body as { through?: number })?.through === 1),
      "Mark read should POST /api/inbox/ack with through=1",
    );
    assert(inboxTab.textContent?.includes("1"), `Unread badge should drop to 1, got ${inboxTab.textContent}`);

    const createIssueBtn = Array.from(listItems[0].querySelectorAll("button")).find(
      (b) => b.textContent?.trim() === "Create issue",
    ) as HTMLButtonElement;
    assert(createIssueBtn, "Create issue button should render when message has no issue");
    await act(async () => {
      createIssueBtn.click();
      await new Promise((r) => setTimeout(r, 20));
    });
    assert(
      calls.some((c) => c.url === "/api/inbox/promote" && (c.body as { message?: string })?.message === "msg-maya-1"),
      "Create issue should POST /api/inbox/promote",
    );

    const issueLink = Array.from(listItems[0].querySelectorAll("button.lnk")).find(
      (b) => b.textContent?.trim() === "CAD-901",
    ) as HTMLButtonElement;
    assert(issueLink, "Promoted issue link CAD-901 should appear");
    await act(async () => {
      issueLink.click();
    });
    assert(openedIssues[0] === "CAD-901", "Clicking promoted issue link should open CAD-901");

    console.log("agentInbox: passed");
  } finally {
    await act(async () => {
      root.unmount();
    });
    globalThis.fetch = origFetch;
  }
}

void main().catch((err) => {
  console.error(err);
  (globalThis as unknown as { process?: { exit: (code: number) => void } }).process?.exit(1);
});
