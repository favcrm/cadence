export {};
/**
 * CAD-2786: Home right-panel Feed revamp tests:
 *  - parses shared AOS-759 fixture & orders attention_required -> working -> monitoring -> completed
 *  - maps running + silent_secs >= 240 to `unverifiable` in fallback projection
 *  - mounts NeedsRail (Feed tab) and verifies clean rest state, Command/Steps fold,
 *    Copy CLI feedback, Ask handoff, RailFeedDrawer detail view, and Live/Poll 15s pill.
 */
import type { Agent, AgentFeedItem, Overview } from "../src/lib/types";
import type { ResourceState } from "../src/lib/cache";

declare function require(name: string): any;

function assert(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(msg);
}

async function flush() {
  for (let i = 0; i < 4; i++) {
    await Promise.resolve();
    await new Promise((r) => setTimeout(r, 0));
  }
}

async function main() {
  const fs = require("fs");
  const path = require("path");
  const processMod = require("process");
  const fixturePath = path.join(processMod.cwd(), "tests/fixtures/aos759-agent-feed.fixture.json");
  const feedItems = JSON.parse(fs.readFileSync(fixturePath, "utf8")) as AgentFeedItem[];
  assert(
    Array.isArray(feedItems) && feedItems.length >= 4,
    "aos759 fixture contains AgentFeedItem[] array",
  );

  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/" });
  for (const name of [
    "window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement",
    "HTMLButtonElement", "SVGElement", "navigator", "MutationObserver",
    "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location",
    "history", "sessionStorage",
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
  let copiedText = "";
  Object.defineProperty(win.navigator, "clipboard", {
    value: { writeText: async (text: string) => { copiedText = text; } },
    configurable: true,
  });
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
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
  const { resources } = require("../src/lib/resources") as typeof import("../src/lib/resources");
  const { allowPrivateReads, resetPrivateSession } = require("../src/lib/privateSession") as typeof import("../src/lib/privateSession");
  const NeedsRail = (require("../src/features/home/NeedsRail") as typeof import("../src/features/home/NeedsRail")).default;
  const {
    buildFallbackFeedItems,
    feedCategoriesForItem,
    mergeFeedItems,
  } = require("../src/features/home/feedRailModel") as typeof import("../src/features/home/feedRailModel");

  const originalFetch = globalThis.fetch;
  try {
    // 1. Model invariants: sorting, action classification, stale heartbeat decay
    const shuffled = [...feedItems].reverse();
    const sorted = mergeFeedItems(shuffled, []);
    assert(
      sorted[0].state === "action_required",
      `attention items sort first (got ${sorted[0].state})`,
    );
    assert(
      feedCategoriesForItem(sorted[0]).includes("action"),
      "action_required card carries `action` filter category",
    );

    const staleAgent: Agent = {
      alias: "stale-worker",
      provider: "codex",
      endpoint_kind: "cli",
      role: "worker",
      team_role: null,
      model: null,
      model_reported: null,
      model_configured: null,
      model_source: null,
      effort: null,
      effort_reported: null,
      effort_source: null,
      effort_applicable: false,
      quota: null,
      usage_limit: null,
      state: "running",
      group: "cadence",
      group_root: false,
      running: 1,
      queued: 0,
      unknown: 0,
      parked: 0,
      fenced: false,
      on: ["CAD-2786"],
      tasks: [],
      message: null,
      dead: false,
      silent_secs: 260,
      last_activity: new Date(Date.now() - 260_000).toISOString(),
    };
    const fallback = buildFallbackFeedItems({
      agents: [staleAgent],
      reportsByAlias: new Map(),
    });
    assert(fallback.length === 1 && fallback[0].state === "unverifiable", "running + silent >= 240s decays to unverifiable");

    // 2. Mounted NeedsRail -> Feed interaction
    globalThis.fetch = (async (input: unknown) => {
      const pathname = new URL(String(input), "http://localhost").pathname;
      if (pathname === "/api/agents") {
        return new Response(
          JSON.stringify({
            daemon: "ok",
            totals: null,
            agents: [],
            feed_items: feedItems,
          }),
          { headers: { "Content-Type": "application/json" } },
        );
      }
      return new Response(JSON.stringify({}), { headers: { "Content-Type": "application/json" } });
    }) as typeof fetch;

    resetPrivateSession();
    allowPrivateReads();
    await React.act(async () => {
      await resources.agents.revalidate();
    });

    const overview: ResourceState<Overview> = {
      status: "ok",
      data: {
        needs_me: [],
        drift: { matched: true },
        projects: [],
        github: { state: "ok" },
        daemon: { reachable: true },
        generated_at: Date.now(),
      },
      error: null,
      asOf: Date.now(),
      inFlight: false,
    };

    const asked: AgentFeedItem[] = [];
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);

    await React.act(async () => {
      root.render(
        React.createElement(
          LocaleProvider,
          null,
          React.createElement(NeedsRail, {
            overview,
            readOnly: false,
            streamDegraded: false,
            onAskFeedItem: (item: AgentFeedItem) => asked.push(item),
          }),
        ),
      );
    });
    await flush();

    // Switch to Feed tab
    const feedTabBtn = Array.from(host.querySelectorAll("button")).find((b) =>
      b.textContent?.includes("Feed"),
    );
    assert(feedTabBtn, "Feed tab button present");
    await React.act(async () => {
      feedTabBtn.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flush();

    const livePill = host.querySelector(".rail-stream-pill");
    assert(livePill?.textContent?.trim() === "Live", `shows Live indicator (got ${livePill?.textContent})`);

    // Clean rest state: advanced disclosures start collapsed
    const cards = Array.from(host.querySelectorAll("li.rc2"));
    assert(cards.length === feedItems.length, `renders all ${feedItems.length} feed cards`);
    const firstFold = cards[0].querySelector(".adv-fold");
    assert(firstFold?.getAttribute("aria-hidden") === "true", "advanced CLI fold starts collapsed at rest");

    // Expand Command -> reveals operator CLI & switches aria-expanded
    const expandBtn = Array.from(cards[0].querySelectorAll("button")).find((b) =>
      b.textContent?.includes("Command"),
    );
    assert(expandBtn, "Command toggle button present on action_required card");
    await React.act(async () => {
      expandBtn.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    assert(expandBtn.getAttribute("aria-expanded") === "true", "Command fold expands on click");
    assert(
      Boolean(cards[0].querySelector(".cmd-pre")?.textContent?.includes("delivery enqueue")),
      "expanded CLI box contains exact operator command",
    );

    // Copy CLI writes to clipboard cleanly and fails closed on clipboard error
    const copyBtn = Array.from(cards[0].querySelectorAll("button")).find((b) =>
      b.textContent?.includes("Copy CLI"),
    );
    assert(copyBtn, "Copy CLI button present");
    await React.act(async () => {
      copyBtn.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flush();
    assert(copiedText.includes("delivery enqueue"), "Copy CLI wrote exact command to clipboard");
    assert(Boolean(copyBtn.textContent?.includes("Copied")), "Copy CLI button reflects Copied state");

    Object.defineProperty(win.navigator, "clipboard", {
      value: { writeText: async () => { throw new Error("clipboard denied"); } },
      configurable: true,
    });
    await React.act(async () => {
      copyBtn.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flush();
    assert(copyBtn.textContent?.trim() === "Copy failed", `Copy CLI fails closed on error (got ${copyBtn.textContent})`);
    assert(!copyBtn.textContent?.includes("Copied"), "Failed clipboard write must never claim Copied");

    // Ask button triggers onAskFeedItem on working item
    const workingCard = cards.find((c) => c.getAttribute("data-feed-state") === "working");
    assert(workingCard, "working card present");
    const askBtn = Array.from(workingCard.querySelectorAll("button")).find((b) =>
      b.textContent?.trim() === "Ask",
    );
    assert(askBtn, "Ask button present on non-operator card");
    await React.act(async () => {
      askBtn.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    assert(asked.length === 1 && asked[0].state === "working", "Ask callback receives the working Feed item");

    // Details opens RailFeedDrawer with operator-only notice
    const detailsBtn = Array.from(cards[0].querySelectorAll("button")).find((b) =>
      b.textContent?.includes("Details"),
    );
    assert(detailsBtn, "Details button present");
    await React.act(async () => {
      detailsBtn.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flush();
    assert(
      Boolean(document.body.textContent?.includes("Operator-only terminal command")),
      "RailFeedDrawer displays operator-only command panel",
    );

    // Rerender with streamDegraded=true -> shows Poll 15s badge
    await React.act(async () => {
      root.render(
        React.createElement(
          LocaleProvider,
          null,
          React.createElement(NeedsRail, {
            overview,
            readOnly: false,
            streamDegraded: true,
            onAskFeedItem: (item: AgentFeedItem) => asked.push(item),
          }),
        ),
      );
    });
    const pollPill = host.querySelector(".rail-stream-pill.warn");
    assert(pollPill?.textContent?.trim() === "Poll 15s", "streamDegraded flips indicator to Poll 15s");

    await React.act(async () => {
      root.unmount();
    });
    host.remove();
    console.log("home feed rail checks passed");
  } finally {
    globalThis.fetch = originalFetch;
    resetPrivateSession();
    win.close();
    loader.prototype.require = originalRequire;
  }
}

void main().catch((err) => {
  console.error(err);
  require("process").exit(1);
});
