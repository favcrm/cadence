export {};
/**
 * CAD-2656: Home rail polish regressions —
 *  1. Mobile `.needsdrawer` Feed pills compact rules & zero drawer body padding;
 *  2. `AgentUpdates` fragment render + `.rail-cards:not([hidden])` boundary collapse;
 *  3. Pending `staged_write` cards never repeat `source · age` in `.rc-summary`;
 *  4. Sidebar `Home` badge (`useHomeCount` + `useHostedTemplatePendingCount`) matches `NeedsRail`;
 *  5. `restoreDetailFocus` restores to `.needbtn` on narrow Home and `.needsrail` tab on wide Home.
 */
import type * as HappyDomModule from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDomModule from "react-dom/client";
import type * as CacheModule from "../src/lib/cache";
import type * as TypesModule from "../src/lib/types";

declare function require(name: string): unknown;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(`homeRailPolish test failed: ${why}`);
}

const cleanText = (el: Element | null): string =>
  el?.textContent?.replace(/[⁨⁩]/g, "").replace(/\s+/g, " ").trim() ?? "";

async function main() {
  const fs = require("fs") as { readFileSync: (file: string, encoding: string) => string };
  const path = require("path") as { join: (...parts: string[]) => string };
  const processMod = require("process") as { cwd: () => string; exitCode?: number };
  const { Window } = require("happy-dom") as typeof HappyDomModule;
  const win = new Window({ url: "http://localhost/" });
  for (const name of [
    "window",
    "document",
    "Node",
    "Element",
    "HTMLElement",
    "HTMLInputElement",
    "HTMLTextAreaElement",
    "HTMLSelectElement",
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
  ] as const) {
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : (win as unknown as Record<string, unknown>)[name],
      configurable: true,
      writable: true,
    });
  }
  for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"] as const) {
    Object.defineProperty(globalThis, name, {
      value: (win as unknown as Record<string, (...a: unknown[]) => unknown>)[name].bind(win),
      configurable: true,
    });
  }
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

  const loader = require("module") as { prototype: { require: (id: string) => unknown } };
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.call(this, id);
  };

  const React = require("react") as typeof ReactModule;
  const { createRoot } = require("react-dom/client") as typeof ReactDomModule;
  const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
  const { allowPrivateReads, pausePrivateReads, resetPrivateSession } =
    require("../src/lib/privateSession") as typeof import("../src/lib/privateSession");
  const { resources } = require("../src/lib/resources") as typeof import("../src/lib/resources");
  const AgentUpdates = (require("../src/features/home/AgentUpdates") as typeof import("../src/features/home/AgentUpdates")).default;
  const TodoCard = (require("../src/features/home/TodoCard") as typeof import("../src/features/home/TodoCard")).default;
  const NeedsRail = (require("../src/features/home/NeedsRail") as typeof import("../src/features/home/NeedsRail")).default;
  const { stagedNeed } = require("../src/features/home/needs") as typeof import("../src/features/home/needs");
  const { parseStagedRow } = require("../src/features/decisions/stagedWrite") as typeof import("../src/features/decisions/stagedWrite");
  const { decideRoutine, resetTodoLocal, useHomeCount } =
    require("../src/features/home/todoLocal") as typeof import("../src/features/home/todoLocal");
  const { useHostedTemplatePendingCount, useHostedTemplateReview } =
    require("../src/features/templates/HostedTemplateReview") as typeof import("../src/features/templates/HostedTemplateReview");
  const { restoreDetailFocus } =
    require("../src/features/work-items/focus") as typeof import("../src/features/work-items/focus");

  const originalFetch = globalThis.fetch;
  const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
  const tick = (ms = 20) => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, ms)); });

  try {
    // ---- 1. Mobile drawer CSS rules in styles.css ----
    const css = fs.readFileSync(path.join(processMod.cwd(), "src/styles.css"), "utf8");
    assert(
      /\.s-drawer\.needsdrawer\s*\.s-drawer-body[\s\S]*padding-inline:\s*0;[\s\S]*scrollbar-gutter:\s*auto;/m.test(css),
      "styles.css zeroes horizontal padding and scrollbar-gutter on .s-drawer.needsdrawer .s-drawer-body",
    );
    assert(
      /@media\s*\(max-width:\s*860px\)\s*\{[\s\S]*\.s-drawer\.needsdrawer\[data-scrim="board"\]\s*\.s-drawer-body\s*\{[\s\S]*padding-inline:\s*0;/m.test(css),
      "styles.css overrides drawer.css mobile horizontal padding on .s-drawer.needsdrawer",
    );
    assert(
      css.includes(".s-drawer.needsdrawer .seg-pill"),
      "styles.css scopes compact Feed pill sizing to .s-drawer.needsdrawer",
    );
    assert(
      css.includes(".rail-cards:not([hidden]) ~ .rail-cards:not([hidden])") &&
        css.includes(".rail-cards:not([hidden]):has(~ .rail-cards:not([hidden]))"),
      "styles.css collapses adjacent visible .rail-cards boundaries without collapsing after [hidden]",
    );
    assert(
      /\.rail-card\s+\.rc-head\s*\{[\s\S]*flex-wrap:\s*nowrap;[\s\S]*margin:\s*0;/m.test(css),
      "styles.css scopes .rail-card .rc-head with margin: 0 and flex-wrap: nowrap",
    );
    assert(
      !/^\.rc-head\s*\{/m.test(css),
      "styles.css does not define an unscoped .rc-head rule that collides with recruit.css",
    );

    // ---- 2. AgentUpdates renders <ul class="rail-cards"> as direct child inside Feed ----
    {
      globalThis.fetch = (async (input: unknown) => {
        const pathname = new URL(String(input), "http://localhost").pathname;
        if (pathname === "/api/agents") {
          return new Response(JSON.stringify({
            daemon: "ok",
            totals: null,
            agents: [{
              alias: "atlas",
              kind: "worker",
              role: "worker",
              running: 1,
              tasks: [],
              issue_id: null,
              issue_title: null,
              last_seen: Math.floor(Date.now() / 1000),
            }],
          }), { headers: { "Content-Type": "application/json" } });
        }
        if (pathname.startsWith("/api/threads/")) {
          return new Response(JSON.stringify({ entries: [] }), { headers: { "Content-Type": "application/json" } });
        }
        if (pathname.startsWith("/api/agents/")) {
          return new Response(JSON.stringify({ events: [] }), { headers: { "Content-Type": "application/json" } });
        }
        return new Response(JSON.stringify({}), { headers: { "Content-Type": "application/json" } });
      }) as typeof fetch;

      resetPrivateSession();
      allowPrivateReads();
      await React.act(async () => { await resources.agents.revalidate(); });
      const host = document.createElement("div");
      document.body.append(host);
      const root = createRoot(host);
      await React.act(async () => {
        root.render(React.createElement(LocaleProvider, null,
          React.createElement(
            "div",
            { "data-feed-wrap": "1" },
            React.createElement(AgentUpdates, { category: "all" }),
            React.createElement("ul", { className: "rail-cards", "data-feed-docs": "" }),
          ),
        ));
      });
      await flush();
      const wrap = host.querySelector("[data-feed-wrap]")!;
      const directClasses = Array.from(wrap.children).map((el) => `${el.tagName.toLowerCase()}.${el.className}`);
      assert(
        directClasses.length === 2 && directClasses[0] === "ul.rail-cards" && directClasses[1] === "ul.rail-cards",
        `AgentUpdates renders <ul class="rail-cards"> as direct sibling before [data-feed-docs] (got ${directClasses.join(", ")})`,
      );
      await React.act(async () => { root.unmount(); });
      host.remove();
    }

    // ---- 3. Pending staged_write card fields never duplicate source · age ----
    {
      const now = Date.now();
      const t = (s: string) => s;
      const appRow = parseStagedRow({
        id: "app-1",
        tool: "app_call",
        status: "pending",
        createdAt: now - 4 * 60_000,
        title: "Update Marlow Books",
        app: "CRM",
        target: { kind: "customer", label: "Marlow Books", id: "cus_1" },
        changes: [
          { field: "stage", label: "Stage", kind: "changed", before: "New", after: "Won" },
          { field: "owner", label: "Owner", kind: "added", after: "Ada" },
        ],
        unchanged_count: 0,
        changes_complete: true,
        args: "{}",
        origin: { via: "cli", kind: "agent", label: "Atlas" },
      })!;
      const mcpRow = parseStagedRow({
        id: "mcp-1",
        tool: "custom_mcp_effect",
        status: "pending",
        createdAt: now - 3 * 60_000,
        origin: { via: "mcp", kind: "agent", label: "Relay" },
        input: {
          connection_id: "conn_1",
          connection_name: "Linear",
          endpoint_host: "mcp.linear.app",
          tool: "create_comment",
          snapshot_revision: 2,
          args: JSON.stringify({ issue: "CAD-1" }),
          preview: {
            kind: "message",
            title: "Reply on CAD-1",
            toolkit_label: "Linear",
            account: "bot@linear",
            fields: [{ role: "body", label: "Body", value: "Ready" }],
          },
        },
      })!;

      const host = document.createElement("div");
      document.body.append(host);
      const root = createRoot(host);
      await React.act(async () => {
        root.render(React.createElement(LocaleProvider, null,
          React.createElement(
            "ul",
            null,
            React.createElement(TodoCard, { need: stagedNeed(appRow, t, now), doneText: undefined, onOpen: () => {} }),
            React.createElement(TodoCard, { need: stagedNeed(mcpRow, t, now), doneText: undefined, onOpen: () => {} }),
          ),
        ));
      });
      await flush();
      const cards = host.querySelectorAll('[data-need="staged_write"]');
      assert(cards.length === 2, "two staged cards rendered");
      assert(
        cleanText(cards[0].querySelector(".rc-source")) === "CRM" &&
          cleanText(cards[0].querySelector(".rc-head-right")) === "4 min" &&
          cleanText(cards[0].querySelector(".todo-meta")) === "Stage, Owner" &&
          cleanText(cards[0].querySelector(".rc-meta")) === "Atlas (agent)",
        "app_call without reason renders changed field labels in summary and asker in footer",
      );
      assert(
        cleanText(cards[1].querySelector(".rc-source")) === "Linear (mcp.linear.app)" &&
          cleanText(cards[1].querySelector(".rc-head-right")) === "3 min" &&
          cleanText(cards[1].querySelector(".todo-meta")) === "Reply on CAD-1" &&
          cleanText(cards[1].querySelector(".rc-meta.ext")) === "External · Relay (agent)",
        "custom_mcp_effect renders preview title in summary and External + asker in footer",
      );
      await React.act(async () => { root.unmount(); });
      host.remove();
    }

    // ---- 4. Home badge parity across needs_me, routines, docs, staged, and draft templates ----
    {
      resetTodoLocal();
      resetPrivateSession();
      allowPrivateReads();

      globalThis.fetch = (async (input: unknown) => {
        const pathname = new URL(String(input), "http://localhost").pathname;
        if (pathname === "/api/templates/list") {
          return new Response(JSON.stringify({
            templates: [{
              name: "Quote",
              latest_version: 2,
              latest_approved_version: 1,
              kind: "quotation",
              source_format: "html",
              status: "draft",
              versions: [{
                name: "Quote",
                version: 2,
                based_on: 1,
                status: "draft",
                kind: "quotation",
                content_sha256: "a".repeat(64),
                source_sha256: "b".repeat(64),
                approved_source: null,
                created: 1,
              }],
            }],
          }), { headers: { "Content-Type": "application/json" } });
        }
        return new Response(JSON.stringify({ actions: [] }), { headers: { "Content-Type": "application/json" } });
      }) as typeof fetch;

      const overviewData: TypesModule.Overview = {
        needs_me: [
          {
            kind: "blocked",
            audience: "operator",
            title: "CAD-1 blocked",
            age: 60,
            project: "cadence",
            command: "cadence",
            subject: { kind: "issue", id: "CAD-1" },
          },
        ],
        document_follow_ups: {
          status: "ready",
          pending: [{
            id: "doc-1",
            title: "Policy",
            version: 1,
            project: "cadence",
            href: "/wiki/policy",
            at: 1,
          }],
          recent: [],
        } as TypesModule.DocumentFollowUps,
        drift: { behind: false, running: "1", disk: "1" } as unknown as TypesModule.Drift,
        projects: [],
        github: { state: "ok" },
        daemon: { reachable: true },
        routine_needs: [{ id: "rt-wait", name: "Morning", status: "unapproved", approval_drift: false, approved_rev: null, approved_sha256: null } as unknown as TypesModule.Routine],
        routine_proposals: [{ id: "rt-prop", title: "Draft routine", kind: "routine_propose", proposed_by: "atlas" } as unknown as TypesModule.StagedRoutineProposal],
        routine_step_holds: [{ id: "rt-hold", routine_id: "r1", agent_alias: "atlas" } as unknown as TypesModule.RoutineRun],
        generated_at: Date.now(),
      };
      const overviewState: CacheModule.ResourceState<TypesModule.Overview> = {
        status: "ok",
        inFlight: false,
        error: null,
        asOf: Date.now(),
        data: overviewData,
      };

      let latestHomeCount = -1;
      const currentHomeCount = () => latestHomeCount;
      function Probe() {
        const other = useHomeCount(overviewData.needs_me, 1, overviewData);
        const tplCount = useHostedTemplatePendingCount();
        const review = useHostedTemplateReview({ enabled: true, canApprove: true });
        latestHomeCount = other + (overviewData.document_follow_ups?.status === "ready" ? overviewData.document_follow_ups.pending.length : 0) + tplCount;
        return React.createElement(NeedsRail, {
          overview: overviewState,
          documentFollowUps: overviewData.document_follow_ups,
          templateRail: review.rail,
          templateCount: review.pendingCount,
          templateList: review.templateList,
          readOnly: false,
        });
      }

      const host = document.createElement("div");
      document.body.append(host);
      const root = createRoot(host);
      await React.act(async () => {
        root.render(React.createElement(LocaleProvider, null, React.createElement(Probe)));
      });
      await tick(30);
      await flush();

      const railCountText = cleanText(host.querySelector(".needsrail .todo-count"));
      // NeedsRail rendered without decisions=true here (0 staged), while Probe passed +1 staged; compare exact routine+doc+tpl+need share:
      assert(railCountText === "6", `NeedsRail To do counts 1 need + 3 routines + 1 doc + 1 draft template = 6 (got "${railCountText}")`);
      assert(currentHomeCount() === 7, `useHomeCount + docs + templatePendingCount includes routines and draft template (got ${currentHomeCount()})`);

      await React.act(async () => { decideRoutine("rt-hold"); });
      await flush();
      assert(cleanText(host.querySelector(".needsrail .todo-count")) === "5", "NeedsRail count decrements immediately when routine hold decided");
      assert(currentHomeCount() === 6, "sidebar homeCount decrements in lockstep when routine hold decided");

      await React.act(async () => { pausePrivateReads(); });
      await flush();
      // While paused, published template count clears to 0
      assert(currentHomeCount() === 5, `pausePrivateReads clears published template count (got ${currentHomeCount()})`);

      await React.act(async () => { root.unmount(); });
      host.remove();
    }

    // ---- 5. restoreDetailFocus falls back on Home (<1100px -> .needbtn, >=1100px -> .needsrail tab) ----
    {
      const host = document.createElement("div");
      document.body.append(host);
      const rail = document.createElement("div");
      rail.className = "needsrail";
      const tabs = document.createElement("div");
      tabs.className = "todo-tabs";
      const tabBtn = document.createElement("button");
      tabBtn.textContent = "To do";
      tabs.append(tabBtn);
      rail.append(tabs);
      const needBtn = document.createElement("button");
      needBtn.className = "needbtn";
      needBtn.textContent = "To do";
      host.append(rail, needBtn);

      const disconnectedCard = document.createElement("button");

      (win as unknown as { innerWidth: number }).innerWidth = 390;
      document.body.focus();
      restoreDetailFocus(disconnectedCard);
      await tick(30);
      assert(document.activeElement === needBtn, "narrow Home focuses visible .needbtn when opener unmounted from .needsdrawer");

      (win as unknown as { innerWidth: number }).innerWidth = 1440;
      document.body.focus();
      restoreDetailFocus(disconnectedCard);
      await tick(30);
      assert(document.activeElement === tabBtn, "wide Home focuses .needsrail .todo-tabs > button, never .needbtn");

      const composer = document.createElement("textarea");
      host.append(composer);
      composer.focus();
      restoreDetailFocus(disconnectedCard);
      await tick(30);
      assert(document.activeElement === composer, "live composer focus is never stolen");

      host.remove();
    }

    console.log("home rail polish checks passed");
  } finally {
    globalThis.fetch = originalFetch;
    resetTodoLocal();
    resetPrivateSession();
    loader.prototype.require = originalRequire;
    win.close();
  }
}

void main().catch((error: unknown) => {
  console.error(error);
  const processMod = require("process") as { exitCode?: number };
  processMod.exitCode = 1;
});
