export {};
/**
 * CAD-2806: Home To do, Detail Drawer, and Notifications bell integration for
 * `hosted_local_assignments`.
 */
import type { HostedLocalAssignmentTodo, Overview } from "../src/lib/types";
import type { ResourceState } from "../src/lib/cache";
import type { NotificationInbox as Inbox } from "../src/features/threads/humanThreadsClient";

declare function require(name: string): any;
declare const process: { exit(code?: number): never };

function assert(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(msg);
}

async function flush() {
  for (let i = 0; i < 6; i++) {
    await Promise.resolve();
    await new Promise((r) => setTimeout(r, 0));
  }
}

async function main() {
  const fs = require("fs");
  const path = require("path");
  const processMod = require("process");
  const fixturePath = path.join(processMod.cwd(), "tests/fixtures/local-assignment-todos.fixture.json");
  const fixtureTodos = JSON.parse(fs.readFileSync(fixturePath, "utf8")) as HostedLocalAssignmentTodo[];
  assert(
    Array.isArray(fixtureTodos) && fixtureTodos.length === 2,
    "shared fixture contains DEMOCOMPANY-45 and DEMOCOMPANY-44 local assignment todos",
  );

  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/" });
  const pointerCtor = win.PointerEvent ?? win.MouseEvent;
  for (const name of [
    "window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement",
    "HTMLButtonElement", "HTMLDialogElement", "SVGElement", "navigator", "MutationObserver",
    "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "PointerEvent", "location",
    "history", "sessionStorage", "localStorage",
  ]) {
    const val = name === "window" ? win : name === "PointerEvent" ? pointerCtor : win[name];
    Object.defineProperty(globalThis, name, {
      value: val,
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
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true, configurable: true });

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
  const { allowPrivateReads, resetPrivateSession } = require("../src/lib/privateSession") as typeof import("../src/lib/privateSession");
  const {
    isLocalAssignmentReviewed,
    localAssignmentFingerprint,
    markLocalAssignmentReviewed,
    pendingLocalAssignmentCount,
    resetTodoLocal,
    useTodoLocal,
  } = require("../src/features/home/todoLocal") as typeof import("../src/features/home/todoLocal");
  const { localAssignmentPresentation, sortUnifiedTodos } = require("../src/features/home/railModel") as typeof import("../src/features/home/railModel");
  const NeedsRail = (require("../src/features/home/NeedsRail") as typeof import("../src/features/home/NeedsRail")).default;
  const NotificationInbox = (require("../src/features/threads/NotificationInbox") as typeof import("../src/features/threads/NotificationInbox")).default;
  const { NotificationToastProvider } = require("../src/features/threads/NotificationToast") as typeof import("../src/features/threads/NotificationToast");

  const originalFetch = globalThis.fetch;
  try {
    resetTodoLocal();
    // 1. Model invariants: sorting & reviewed fingerprint generation bump
    const sorted = sortUnifiedTodos([
      { kind: "local_assignment", key: "hla:DEMOCOMPANY-44", category: "blocked", item: fixtureTodos[1]! },
      { kind: "local_assignment", key: "hla:DEMOCOMPANY-45", category: "reviews", item: fixtureTodos[0]! },
    ]);
    assert(sorted[0]?.kind === "local_assignment" && sorted[0].item.issue_id === "DEMOCOMPANY-45", "reviews sorts before blocked");
    assert(localAssignmentPresentation(fixtureTodos[1]!).tone === "fail", "blocked uses fail tone");
    assert(localAssignmentPresentation(fixtureTodos[0]!).cueLabel === "Review", "result uses Review cue");

    const g1 = fixtureTodos[0]!;
    const g2: HostedLocalAssignmentTodo = {
      ...g1,
      assignment: { ...g1.assignment, generation: 2 },
    };
    markLocalAssignmentReviewed(g1);
    assert(pendingLocalAssignmentCount([g1, fixtureTodos[1]!], useTodoLocalSnapshot()) === 1, "marked reviewed decrements count");
    assert(pendingLocalAssignmentCount([g2, fixtureTodos[1]!], useTodoLocalSnapshot()) === 2, "bumped generation re-surfaces ticket");
    resetTodoLocal();

    function useTodoLocalSnapshot() {
      let current: ReturnType<typeof useTodoLocal> | null = null;
      function Probe() {
        current = useTodoLocal();
        return null;
      }
      const el = document.createElement("div");
      const r = createRoot(el);
      React.act(() => { r.render(React.createElement(Probe)); });
      React.act(() => { r.unmount(); });
      return current!;
    }

    // 2. NeedsRail + RailTodoDrawer (Blocked Retry + Reviews Mark reviewed)
    allowPrivateReads();
    const overview: ResourceState<Overview> = {
      status: "ok",
      data: {
        needs_me: [],
        local_assignment_todos: fixtureTodos,
        drift: { matched: true },
        projects: [],
        github: { state: "unavailable" },
        daemon: { reachable: false },
        generated_at: 1,
      },
      error: null,
      asOf: Date.now(),
      inFlight: false,
    };

    const openedIssues: string[] = [];
    const toasts: string[] = [];
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
            canLocalDispatch: true,
            scopedHost: true,
            hasLiveAgents: true,
            onOpenIssue: (id: string) => openedIssues.push(id),
            onToast: (msg: string) => toasts.push(msg),
          }),
        ),
      );
      await flush();
    });

    assert(host.textContent?.includes("DEMOCOMPANY-45 · Local worker finished Turn 1"), "rail renders result todo card");
    assert(host.textContent?.includes("DEMOCOMPANY-44 · Local kickoff blocked on allowlist"), "rail renders blocked todo card");
    // Open Blocked card drawer and confirm Retry & Cancel buttons render inside drawer (never on card)
    const cardButtons = host.querySelectorAll('.needrow button.rail-card-action');
    assert(cardButtons.length === 0, "no inline action buttons on rail cards");

    const blockedHit = host.querySelector('[data-local-assignment-issue="DEMOCOMPANY-44"] button.rc-title') as HTMLButtonElement | null;
    assert(blockedHit, "blocked card hit target exists");
    await React.act(async () => {
      blockedHit.click();
      await flush();
    });
    const drawerText = document.body.textContent ?? "";
    assert(drawerText.includes("Retry") && drawerText.includes("Cancel"), "blocked drawer renders HostedLocalAssignmentCard Retry & Cancel");

    // Close drawer, then open Reviews card and Mark reviewed
    const closeBtn = document.querySelector('[data-drawer="todo-detail"] button[aria-label*="Close"], .s-drawer button[aria-label*="Close"]') as HTMLButtonElement | null;
    if (closeBtn) {
      await React.act(async () => { closeBtn.click(); await flush(); });
    }
    const reviewHit = host.querySelector('[data-local-assignment-issue="DEMOCOMPANY-45"] button.rc-title') as HTMLButtonElement | null;
    assert(reviewHit, "review card hit target exists");
    await React.act(async () => {
      reviewHit.click();
      await flush();
    });
    const markReviewedBtn = [...document.querySelectorAll("button")].find((b) => b.textContent?.trim() === "Mark reviewed") as HTMLButtonElement | undefined;
    assert(markReviewedBtn, "review drawer shows Mark reviewed");
    await React.act(async () => {
      markReviewedBtn.click();
      await flush();
    });
    assert(isLocalAssignmentReviewed(useTodoLocalSnapshot(), g1), "Mark reviewed stores fingerprint");
    assert(toasts.some((msg) => msg.includes("DEMOCOMPANY-45")), "Mark reviewed triggers feedback toast");
    assert(!host.querySelector('[data-local-assignment-issue="DEMOCOMPANY-45"]'), "reviewed card leaves To do list");

    React.act(() => root.unmount());
    host.remove();
    resetTodoLocal();

    // 3. NotificationInbox integration: badge count, click opens issue, Mark all read, new toast after hydration
    const inboxPayload: Inbox = {
      human: { id: "human-demo-1", name: "Chris" },
      unread_total: 0,
      groups: [],
      preferences: { dm: true, mention: true, sound: false, preview: true, quiet: "off", zone: "UTC" },
      push: { available: false, public_key: null },
    };
    globalThis.fetch = async (input: RequestInfo | URL) => {
      const path = new URL(input instanceof Request ? input.url : String(input), "http://localhost").pathname;
      if (path === "/api/human-threads/notifications") {
        return new Response(JSON.stringify(inboxPayload), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        });
      }
      throw new Error(`Unexpected fetch ${path}`);
    };

    const bellHost = document.createElement("div");
    document.body.append(bellHost);
    const bellRoot = createRoot(bellHost);
    const bellIssues: string[] = [];

    const renderBell = async (todos: HostedLocalAssignmentTodo[], epoch = "e1") => {
      await React.act(async () => {
        bellRoot.render(
          React.createElement(
            LocaleProvider,
            null,
            React.createElement(
              NotificationToastProvider,
              null,
              React.createElement(NotificationInbox, {
                authEpoch: epoch,
                localAssignmentTodos: todos,
                onOpenIssue: (id: string) => bellIssues.push(id),
              }),
            ),
          ),
        );
        await flush();
      });
    };

    await renderBell(fixtureTodos, "e1");
    assert(bellHost.querySelector(".notification-count")?.textContent === "2", "bell badge counts 2 unread local assignments");
    // No toast on initial hydration
    assert(!document.body.querySelector(".notification-toast"), "no toast spammed on initial hydration");

    // New item arriving after hydration fires toast and increments badge to 3
    const arriving: HostedLocalAssignmentTodo = {
      issue_id: "DEMOCOMPANY-46",
      issue_title: "Resumed lane finished",
      project: "demo-company",
      category: "reviews",
      runner_label: "local · pi",
      assignment: {
        ...fixtureTodos[0]!.assignment,
        assignment_id: "hla_46000000-0000-4000-8000-000000000046",
        issue_id: "DEMOCOMPANY-46",
        text: "TURN_2_RESUMED",
      },
    };
    await renderBell([arriving, ...fixtureTodos], "e1");
    assert(bellHost.querySelector(".notification-count")?.textContent === "3", "bell badge increments to 3");
    assert(document.body.textContent?.includes("DEMOCOMPANY-46"), "toast surfaces newly arrived assignment");

    // Click bell -> click DEMOCOMPANY-46 alert -> opens issue & decrements count to 2
    const bellBtn = bellHost.querySelector("button.bell-button") as HTMLButtonElement | null;
    assert(bellBtn, "bell button exists");
    await React.act(async () => {
      bellBtn.click();
      await flush();
    });
    const alert46Btn = bellHost.querySelector('[data-local-assignment-alert="DEMOCOMPANY-46"] button') as HTMLButtonElement | null;
    assert(alert46Btn, "popover renders alert for DEMOCOMPANY-46");
    await React.act(async () => {
      alert46Btn.click();
      await flush();
    });
    assert(bellIssues[0] === "DEMOCOMPANY-46", "clicking alert navigates to issue");
    assert(bellHost.querySelector(".notification-count")?.textContent === "2", "clicking alert marks it read");

    // Open bell -> Mark all read -> clears remaining 2 alerts cleanly
    await React.act(async () => {
      bellBtn.click();
      await flush();
    });
    const markAllBtn = [...bellHost.querySelectorAll("button")].find((b) => b.textContent?.includes("Mark all read")) as HTMLButtonElement | undefined;
    assert(markAllBtn && !markAllBtn.disabled, "Mark all read is enabled");
    await React.act(async () => {
      markAllBtn.click();
      await flush();
    });
    assert(!bellHost.querySelector(".notification-count"), "Mark all read clears bell badge");
    assert(localAssignmentFingerprint(arriving).includes("hla_46000000"), "fingerprint helper exported cleanly");

    React.act(() => bellRoot.unmount());
    bellHost.remove();
    resetPrivateSession();
    console.log("homeLocalAssignmentUpdates checks passed");
  } finally {
    globalThis.fetch = originalFetch;
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
