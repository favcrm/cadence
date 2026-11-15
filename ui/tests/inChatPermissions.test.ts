/**
 * CAD-2318 checks: inline TurnBar permission decisions & keyboard navigation,
 * PermissionCard Reject & reply draft preservation, Composer ArrowUp empty
 * focus transfer, and the /permissions slash card (two-step revoke, WriteGate
 * enforcement, and 401/403 fail-closed cache clearing).
 */
export {};
declare function require(name: string): any;

function assert(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(msg);
}

async function flush(ticks = 5) {
  for (let i = 0; i < ticks; i++) {
    await Promise.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
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
    "HTMLButtonElement",
    "HTMLInputElement",
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
    "localStorage",
  ]) {
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  }
  for (const name of ["addEventListener", "removeEventListener", "getComputedStyle"]) {
    if (typeof win[name] === "function") {
      Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });
    }
  }
  Object.defineProperty(globalThis, "requestAnimationFrame", {
    value: (cb: (t: number) => void) => setTimeout(() => cb(Date.now()), 0),
    configurable: true,
    writable: true,
  });
  Object.defineProperty(globalThis, "cancelAnimationFrame", {
    value: (id: number) => clearTimeout(id),
    configurable: true,
    writable: true,
  });
  Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true, configurable: true });

  const loader = require("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, arguments);
  };

  interface RecordedCall {
    method: string;
    path: string;
    body: unknown;
  }
  const calls: RecordedCall[] = [];
  let permGetMode: "ok" | "403" = "ok";
  let permPostStatus: 200 | 401 | 403 = 200;

  const makeReq = (id = "perm-1", command = "git push origin feat", prefix: string[] | null = ["git", "push"]) => ({
    id,
    command,
    cwd: "/repo",
    reason: "Push branch for review",
    risk: "medium",
    status: "pending",
    expires_at: Math.floor(Date.now() / 1000) + 300,
    prefix,
  });

  const savedRules = [
    {
      id: "rule-1",
      effect: "allow" as const,
      scope: "exact" as const,
      argv: ["git", "status"],
      tail: [],
      cwd: "/repo",
      by: "operator",
      at: 1700000000,
    },
    {
      id: "rule-2",
      effect: "deny" as const,
      scope: "prefix" as const,
      argv: ["rm", "-rf"],
      tail: ["/tmp/demo"],
      cwd: "",
      by: "operator",
      at: 1700000100,
    },
  ];

  Object.defineProperty(globalThis, "fetch", {
    configurable: true,
    writable: true,
    value: async (input: string | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input.toString();
      const path = new URL(url, "http://localhost").pathname;
      const method = (init?.method ?? "GET").toUpperCase();
      let parsedBody: unknown = null;
      if (typeof init?.body === "string" && init.body.length > 0) {
        try {
          parsedBody = JSON.parse(init.body);
        } catch {
          parsedBody = init.body;
        }
      }
      calls.push({ method, path, body: parsedBody });

      if (method === "GET" && path === "/api/master/permissions") {
        if (permGetMode === "403") {
          return new Response(JSON.stringify({ error: "Operator session required" }), {
            status: 403,
            headers: { "content-type": "application/json" },
          });
        }
        return new Response(
          JSON.stringify({ requests: [makeReq("perm-live", "npm test", ["npm"])], rules: savedRules }),
          { status: 200, headers: { "content-type": "application/json" } },
        );
      }

      if (method === "POST" && path.startsWith("/api/master/permissions/")) {
        if (permPostStatus !== 200) {
          return new Response(JSON.stringify({ error: "Operator auth required" }), {
            status: permPostStatus,
            headers: { "content-type": "application/json" },
          });
        }
        return new Response(JSON.stringify({ ok: true }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      }

      return new Response(JSON.stringify({}), { status: 200, headers: { "content-type": "application/json" } });
    },
  });

  const React = require("react") as typeof import("react");
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const { TurnBar } = require("../src/features/app-shell/chat/TurnBar") as typeof import("../src/features/app-shell/chat/TurnBar");
  const { turnBarState } = require("../src/features/app-shell/chat/turnBarState") as typeof import("../src/features/app-shell/chat/turnBarState");
  const {
    default: PermissionCard,
    PermissionEnabledContext,
    PermissionReplyContext,
    ThreadPermission,
    appendRejectReplyDraft,
    cardFromRequest,
    publishPermission,
    rejectReplyPrompt,
    subscribePermission,
  } = require("../src/features/home/PermissionCard") as typeof import("../src/features/home/PermissionCard");
  const PermissionsSlashCard = (
    require("../src/features/home/PermissionsSlashCard") as typeof import("../src/features/home/PermissionsSlashCard")
  ).default;
  const SharedComposer = (
    require("../src/features/app-shell/chat/Composer") as typeof import("../src/features/app-shell/chat/Composer")
  ).default;
  const { SLASH_COMMANDS } = require("../src/features/home/master") as typeof import("../src/features/home/master");
  const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
  const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof import("../screen-ui/src/navigation");
  const { WriteGate } = require("../src/features/auth/WriteGate") as typeof import("../src/features/auth/WriteGate");
  const { setComposerDraft } = require("../src/features/app-shell/chat/composerStore") as typeof import("../src/features/app-shell/chat/composerStore");

  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);

  const settle = async () => {
    await React.act(async () => {
      await flush(6);
    });
  };
  const render = async (node: import("react").ReactNode) => {
    const inner = React.createElement(LocaleProvider, { children: node }, node);
    await React.act(async () => {
      root.render(
        React.createElement(
          NavigationProvider,
          { adapter: { navigate: () => undefined, href: (h: string) => h, modifiedClicks: "browser" }, children: inner },
          inner,
        ),
      );
    });
    await settle();
  };

  // ---- 1. rejectReplyPrompt & appendRejectReplyDraft ----
  assert(rejectReplyPrompt("npm test") === "Instead of `npm test`: ", "backtick wraps plain command");
  assert(rejectReplyPrompt("echo `hi`") === 'Instead of "echo `hi`": ', "falls back to quotes when command has backtick");
  assert(
    appendRejectReplyDraft("Check build first", "npm test") === "Check build first\nInstead of `npm test`: ",
    "appends steering prompt without discarding existing draft",
  );
  assert(appendRejectReplyDraft("   ", "git status") === "Instead of `git status`: ", "whitespace-only draft replaced cleanly");

  // ---- 2. TurnBar inline controls, keyboard navigation & guards ----
  let reviewCalls = 0 as number;
  let focusComposerCalls = 0 as number;
  let rejectReplyReq: string | null = null;

  const renderTurnBar = async (opts: {
    readOnly?: boolean;
    reconnecting?: boolean;
    gate?: string | null;
    prefix?: string[] | null;
    id?: string;
  }) => {
    const req = makeReq(opts.id ?? "perm-1", "git push origin feat", opts.prefix === undefined ? ["git", "push"] : opts.prefix);
    const bar = React.createElement(TurnBar, {
      state: { kind: "approval" },
      reconnecting: opts.reconnecting ?? false,
      actions: { onReview: () => { reviewCalls += 1; } },
      permission: {
        request: req,
        readOnly: opts.readOnly ?? false,
        onRejectAndReply: (r) => { rejectReplyReq = r.command; },
        onFocusComposer: () => { focusComposerCalls += 1; },
      },
    });
    const node = opts.gate !== undefined
      ? React.createElement(WriteGate.Provider, { value: opts.gate }, bar)
      : bar;
    await render(node);
  };

  await renderTurnBar({});
  const barEl = host.querySelector<HTMLElement>("[data-turn-bar][data-state='needs_approval']");
  assert(barEl !== null, "TurnBar renders needs_approval state");
  assert(host.querySelector("[data-turn-risk='medium']") !== null, "TurnBar renders risk badge");

  // Click Allow once
  calls.length = 0;
  const allowOnceBtn = host.querySelector<HTMLButtonElement>("[data-turn-allow-once]");
  assert(allowOnceBtn !== null, "Allow once button present");
  await React.act(async () => { allowOnceBtn.click(); });
  await settle();
  assert(calls.some((c) => c.method === "POST" && c.path === "/api/master/permissions/perm-1/allow-once"), "Allow once posts allow-once");

  // Open Always submenu and click Prefix
  calls.length = 0;
  const alwaysToggle = host.querySelector<HTMLButtonElement>("[data-turn-always-toggle]");
  assert(alwaysToggle !== null, "Always toggle present");
  await React.act(async () => { alwaysToggle.click(); });
  const prefixBtn = host.querySelector<HTMLButtonElement>("[data-turn-always-prefix]");
  assert(prefixBtn !== null, "Prefix option appears when request has prefix");
  await React.act(async () => { prefixBtn.click(); });
  await settle();
  assert(
    calls.some(
      (c) =>
        c.method === "POST"
        && c.path === "/api/master/permissions/perm-1/always"
        && JSON.stringify(c.body) === JSON.stringify({ scope: "prefix", tail: ["git", "push"] }),
    ),
    "Always -> Prefix posts prefix scope and tail",
  );

  // Open Always submenu and verify Escape closes submenu and restores focus to Always toggle without focusing composer
  await React.act(async () => { alwaysToggle.click(); });
  const exactBtn = host.querySelector<HTMLButtonElement>("[data-turn-always-exact]");
  assert(exactBtn !== null, "Exact command option present in Always menu");
  exactBtn.focus();
  const focusBeforeEsc = focusComposerCalls;
  await React.act(async () => {
    exactBtn.dispatchEvent(new win.KeyboardEvent("keydown", { key: "Escape", bubbles: true }) as unknown as Event);
  });
  assert(host.querySelector("[data-turn-always-menu]") === null, "Escape closes Always submenu");
  assert(document.activeElement === alwaysToggle, "Escape restores focus to Always toggle");
  assert(focusComposerCalls === focusBeforeEsc, "first Escape closing submenu does not jump to composer");

  // Second Escape inside TurnBar moves focus to composer
  await React.act(async () => {
    alwaysToggle.dispatchEvent(new win.KeyboardEvent("keydown", { key: "Escape", bubbles: true }) as unknown as Event);
  });
  assert(focusComposerCalls === focusBeforeEsc + 1, "second Escape inside TurnBar calls onFocusComposer");

  // ArrowDown inside TurnBar returns focus to composer
  await React.act(async () => {
    allowOnceBtn.dispatchEvent(new win.KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }) as unknown as Event);
  });
  assert(focusComposerCalls === focusBeforeEsc + 2, "ArrowDown inside TurnBar calls onFocusComposer");

  // Deny menu -> Reject & reply… + Don't ask again
  calls.length = 0;
  rejectReplyReq = null;
  const denyToggle = host.querySelector<HTMLButtonElement>("[data-turn-deny-toggle]");
  assert(denyToggle !== null, "Deny toggle present");
  await React.act(async () => { denyToggle.click(); });
  const replyBtn = host.querySelector<HTMLButtonElement>("[data-turn-reject-reply]");
  assert(replyBtn !== null, "Reject & reply option present in Deny menu");
  await React.act(async () => { replyBtn.click(); });
  await settle();
  assert(
    calls.some(
      (c) =>
        c.method === "POST"
        && c.path === "/api/master/permissions/perm-1/reject"
        && JSON.stringify(c.body) === JSON.stringify({ dont_ask_again: false }),
    ),
    "Reject & reply posts reject with dont_ask_again=false",
  );
  assert(rejectReplyReq === "git push origin feat", "Reject & reply seeds composer via callback");

  calls.length = 0;
  await React.act(async () => { denyToggle.click(); });
  const neverBtn = host.querySelector<HTMLButtonElement>("[data-turn-reject-never]");
  assert(neverBtn !== null, "Don't ask again button present");
  await React.act(async () => { neverBtn.click(); });
  await settle();
  assert(
    calls.some(
      (c) =>
        c.method === "POST"
        && c.path === "/api/master/permissions/perm-1/reject"
        && JSON.stringify(c.body) === JSON.stringify({ dont_ask_again: true }),
    ),
    "Don't ask again posts reject with dont_ask_again=true",
  );

  // Focus-scoped shortcuts: y, 2, 3, n, r
  const fireKey = async (key: string) => {
    await React.act(async () => {
      allowOnceBtn.dispatchEvent(new win.KeyboardEvent("keydown", { key, bubbles: true }) as unknown as Event);
    });
    await settle();
  };
  calls.length = 0;
  await fireKey("y");
  assert(calls.some((c) => c.path === "/api/master/permissions/perm-1/allow-once"), "y key triggers allow-once");
  calls.length = 0;
  await fireKey("2");
  assert(calls.some((c) => c.path === "/api/master/permissions/perm-1/always" && (c.body as { scope: string }).scope === "exact"), "2 key triggers exact always");
  calls.length = 0;
  await fireKey("3");
  assert(calls.some((c) => c.path === "/api/master/permissions/perm-1/always" && (c.body as { scope: string }).scope === "prefix"), "3 key triggers prefix always");
  calls.length = 0;
  await fireKey("n");
  assert(calls.some((c) => c.path === "/api/master/permissions/perm-1/reject" && (c.body as { dont_ask_again: boolean }).dont_ask_again === false), "n key triggers reject once");
  const reviewBefore = reviewCalls;
  await fireKey("r");
  assert(reviewCalls === reviewBefore + 1, "r key calls Review");

  // Regression: SSE moving TurnBar to idle while Reject & reply POST is in flight must still run onRejectAndReply
  rejectReplyReq = null;
  let releaseReject!: () => void;
  const origFetch = globalThis.fetch;
  globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input.toString();
    if (url.includes("/reject")) {
      await new Promise<void>((resolve) => {
        releaseReject = resolve;
      });
      return new Response(JSON.stringify({ ok: true }), { status: 200, headers: { "content-type": "application/json" } });
    }
    return origFetch(input, init);
  };
  await React.act(async () => {
    host.querySelector<HTMLButtonElement>("[data-turn-deny-toggle]")!.click();
  });
  await React.act(async () => {
    host.querySelector<HTMLButtonElement>("[data-turn-reject-reply]")!.click();
  });
  await render(
    React.createElement(TurnBar, {
      state: { kind: "idle" },
      reconnecting: false,
      actions: { onReview: () => undefined },
      permission: null,
    }),
  );
  releaseReject();
  await settle();
  globalThis.fetch = origFetch;
  assert(
    rejectReplyReq === "git push origin feat",
    "Reject & reply still seeds composer when stream moves TurnBar to idle before POST resolves",
  );

  // Guard checks: readOnly, WriteGate, reconnecting must withhold decision buttons and ignore hotkeys
  for (const guarded of [
    { readOnly: true },
    { readOnly: false, gate: "Read-only session" },
    { readOnly: false, reconnecting: true },
  ]) {
    await renderTurnBar(guarded);
    assert(host.querySelector("[data-turn-allow-once]") === null, `guard ${JSON.stringify(guarded)} hides Allow once`);
    assert(host.querySelector("[data-turn-always-toggle]") === null, `guard ${JSON.stringify(guarded)} hides Always`);
    assert(host.querySelector("[data-turn-deny-toggle]") === null, `guard ${JSON.stringify(guarded)} hides Deny`);
    calls.length = 0;
    const node = host.querySelector<HTMLElement>("[data-turn-bar]")!;
    await React.act(async () => {
      node.dispatchEvent(new win.KeyboardEvent("keydown", { key: "1", bubbles: true }) as unknown as Event);
    });
    await settle();
    assert(calls.length === 0, `guard ${JSON.stringify(guarded)} ignores shortcut keys`);
  }

  // ---- 3. PermissionCard Reject & reply via PermissionReplyContext ----
  let cardReplyArg: string | null = null;
  calls.length = 0;
  await render(
    React.createElement(
      PermissionReplyContext.Provider,
      { value: (cmd: string) => { cardReplyArg = cmd; } },
      React.createElement(PermissionCard, {
        card: cardFromRequest(makeReq("perm-card", "cargo test", ["cargo"])),
        readOnly: false,
        onDone: () => undefined,
      }),
    ),
  );
  const denyCardToggle = Array.from(host.querySelectorAll<HTMLButtonElement>("button")).find((b) =>
    b.textContent?.includes("Reject"),
  );
  assert(denyCardToggle !== undefined, "PermissionCard renders Reject trigger");
  await React.act(async () => { denyCardToggle.click(); });
  const cardReplyBtn = Array.from(host.querySelectorAll<HTMLButtonElement>("button")).find((b) =>
    b.textContent?.includes("Reject & reply…"),
  );
  assert(cardReplyBtn !== undefined, "PermissionCard Deny menu includes Reject & reply…");
  await React.act(async () => { cardReplyBtn.click(); });
  await settle();
  assert(
    calls.some((c) => c.method === "POST" && c.path === "/api/master/permissions/perm-card/reject"),
    "PermissionCard Reject & reply posts reject",
  );
  assert(cardReplyArg === "cargo test", "PermissionCard Reject & reply invokes context callback");

  // ---- 4. Composer onArrowUpEmpty fires only on empty draft with closed slash menu ----
  const arrowUpState = { count: 0 };
  const expectArrowCount = (expected: number, msg: string) => {
    assert(arrowUpState.count === expected, `${msg} (got ${arrowUpState.count})`);
  };
  const storeKey = "test-arrow-up-empty";
  setComposerDraft(storeKey, "");
  await render(
    React.createElement(SharedComposer, {
      density: "full",
      storeKey,
      block: null,
      commands: SLASH_COMMANDS,
      onCommand: () => true,
      onSend: () => undefined,
      onSubmitted: () => undefined,
      onArrowUpEmpty: () => {
        arrowUpState.count += 1;
        return true;
      },
      textareaId: "test-composer-area",
      ariaLabel: "Message",
      placeholder: "Write...",
    }),
  );
  const textarea = host.querySelector<HTMLTextAreaElement>("#test-composer-area")!;
  assert(textarea !== null, "Composer textarea rendered");
  const typeInto = async (text: string) => {
    const setter = Object.getOwnPropertyDescriptor(win.HTMLTextAreaElement.prototype, "value")!.set!;
    await React.act(async () => {
      setter.call(textarea, text);
      textarea.dispatchEvent(new win.Event("input", { bubbles: true }) as unknown as Event);
    });
    await settle();
  };
  await React.act(async () => {
    textarea.dispatchEvent(new win.KeyboardEvent("keydown", { key: "ArrowUp", bubbles: true }) as unknown as Event);
  });
  expectArrowCount(1, "ArrowUp on empty composer triggers onArrowUpEmpty");

  // Non-empty draft: ArrowUp does not focus approval bar
  await typeInto("draft note");
  await React.act(async () => {
    textarea.dispatchEvent(new win.KeyboardEvent("keydown", { key: "ArrowUp", bubbles: true }) as unknown as Event);
  });
  expectArrowCount(1, "ArrowUp with non-empty draft does not trigger onArrowUpEmpty");

  // Open slash menu (`/`): ArrowUp does not trigger onArrowUpEmpty
  await typeInto("/");
  assert(host.querySelector("[role='listbox']") !== null, "slash menu opens on `/`");
  await React.act(async () => {
    textarea.dispatchEvent(new win.KeyboardEvent("keydown", { key: "ArrowUp", bubbles: true }) as unknown as Event);
  });
  expectArrowCount(1, "ArrowUp with open slash menu does not trigger onArrowUpEmpty");

  // Clear back to empty and press ArrowUp -> fires again
  await typeInto("");
  await React.act(async () => {
    textarea.dispatchEvent(new win.KeyboardEvent("keydown", { key: "ArrowUp", bubbles: true }) as unknown as Event);
  });
  expectArrowCount(2, "ArrowUp fires again once draft is empty");

  // ---- 5. PermissionsSlashCard: pending + rules + two-step revoke + WriteGate + 403 clearing ----
  permGetMode = "ok";
  calls.length = 0;
  await render(
    React.createElement(PermissionsSlashCard, {
      readOnly: false,
      permissionsHref: "/settings#master-permissions",
    }),
  );
  assert(host.querySelector("[data-permissions-card]") !== null, "PermissionsSlashCard mounts");
  assert(host.querySelector("[data-permissions-pending='perm-live']") !== null, "renders pending permission row");
  assert(host.querySelector("[data-permission-rule='rule-1']") !== null, "renders saved rule-1");
  assert(host.querySelector("[data-permission-rule='rule-2']") !== null, "renders saved rule-2");
  assert(host.querySelector("a[href='/settings#master-permissions']") !== null, "renders Open full permissions link");

  // Two-step revoke: first click arms confirm (zero POSTs), Escape cancels
  calls.length = 0;
  const revoke1 = host.querySelector<HTMLButtonElement>("[data-permission-revoke='rule-1']")!;
  await React.act(async () => { revoke1.click(); });
  const confirm1 = host.querySelector<HTMLButtonElement>("[data-permission-confirm-revoke='rule-1']");
  assert(confirm1 !== null, "first click shows Confirm revoke");
  assert(!calls.some((c) => c.method === "POST"), "first click sends no POST");

  const ruleRow1 = host.querySelector<HTMLElement>("[data-permission-rule='rule-1']")!;
  await React.act(async () => {
    ruleRow1.dispatchEvent(new win.KeyboardEvent("keydown", { key: "Escape", bubbles: true }) as unknown as Event);
  });
  assert(host.querySelector("[data-permission-confirm-revoke='rule-1']") === null, "Escape cancels two-step revoke");
  assert(!calls.some((c) => c.method === "POST"), "Escape sends no POST");

  // Confirm revoke -> POST /api/master/permissions/rules/rule-1/revoke
  await React.act(async () => { host.querySelector<HTMLButtonElement>("[data-permission-revoke='rule-1']")!.click(); });
  await React.act(async () => { host.querySelector<HTMLButtonElement>("[data-permission-confirm-revoke='rule-1']")!.click(); });
  await settle();
  assert(calls.some((c) => c.method === "POST" && c.path === "/api/master/permissions/rules/rule-1/revoke"), "Confirm revoke posts revoke");

  // HTTP 403 fail-closed clears cached rules
  permGetMode = "403";
  await React.act(async () => { publishPermission(); });
  await settle();
  assert(host.querySelectorAll("[data-permission-rule]").length === 0, "403 clears previously cached permission rules");
  assert(host.querySelector("[role='alert']") !== null, "403 surfaces auth error");

  // WriteGate disables Revoke even when readOnly=false
  permGetMode = "ok";
  await render(
    React.createElement(
      WriteGate.Provider,
      { value: "Operator locked" },
      React.createElement(PermissionsSlashCard, { readOnly: false }),
    ),
  );
  const gatedRevoke = host.querySelector<HTMLButtonElement>("[data-permission-revoke='rule-1']");
  assert(gatedRevoke !== null && gatedRevoke.disabled, "WriteGate disables Revoke button");

  // ---- 6. CAD-2319: confirmRevoke POST 403 clears cached rules & confirm state ----
  permGetMode = "ok";
  permPostStatus = 200;
  await render(
    React.createElement(PermissionsSlashCard, {
      readOnly: false,
      permissionsHref: "/settings#master-permissions",
    }),
  );
  assert(host.querySelector("[data-permission-rule='rule-1']") !== null, "rules loaded before POST 403 check");
  await React.act(async () => { host.querySelector<HTMLButtonElement>("[data-permission-revoke='rule-1']")!.click(); });
  assert(host.querySelector("[data-permission-confirm-revoke='rule-1']") !== null, "confirm armed before POST 403 check");
  permPostStatus = 403;
  await React.act(async () => { host.querySelector<HTMLButtonElement>("[data-permission-confirm-revoke='rule-1']")!.click(); });
  await settle();
  assert(host.querySelectorAll("[data-permission-rule]").length === 0, "confirmRevoke 403 clears cached saved rules");
  assert(host.querySelectorAll("[data-permissions-pending]").length === 0, "confirmRevoke 403 clears cached pending items");
  assert(host.querySelector("[data-permission-confirm-revoke]") === null, "confirmRevoke 403 clears confirmId");
  assert(host.querySelector("[role='alert']")?.textContent?.includes("Operator auth required"), "confirmRevoke 403 keeps error alert visible");

  // ---- 7. CAD-2319: TurnBar runDecision 401/403 closes menus & notifies permission listeners ----
  permPostStatus = 200;
  await renderTurnBar({ id: "perm-auth", readOnly: false });
  await React.act(async () => { host.querySelector<HTMLButtonElement>("[data-turn-always-toggle]")!.click(); });
  assert(host.querySelector("[data-turn-always-menu]") !== null, "Always menu open before 401 test");
  let publishedOnAuthLoss = 0;
  const offPerm = subscribePermission(() => { publishedOnAuthLoss += 1; });
  permPostStatus = 401;
  await React.act(async () => { host.querySelector<HTMLButtonElement>("[data-turn-always-exact]")!.click(); });
  await settle();
  offPerm();
  permPostStatus = 200;
  assert(publishedOnAuthLoss >= 1, "TurnBar POST 401 calls publishPermission immediately");
  assert(host.querySelector("[data-turn-always-menu]") === null, "TurnBar POST 401 closes open Always menu");
  assert(host.querySelector("[role='alert']")?.textContent?.includes("Operator auth required"), "TurnBar POST 401 shows alert");

  // ---- 8. CAD-2319: <bdi> isolation + control/bidi stripping across TurnBar and PermissionCard ----
  const dirty = "git\u202E status\u2066\x1b[31m\nfoo";
  const cleanExpected = "git status[31m foo";
  await render(
    React.createElement(TurnBar, {
      state: { kind: "approval" },
      reconnecting: false,
      actions: { onReview: () => undefined },
      permission: {
        request: makeReq("perm-bidi", dirty, ["git\u202E", "status\u2069"]),
        readOnly: false,
      },
    }),
  );
  const cmdBdi = host.querySelector(".tb-cmd bdi");
  assert(cmdBdi !== null && cmdBdi.textContent === cleanExpected, `TurnBar command strips bidi/control and wraps <bdi> (got ${JSON.stringify(cmdBdi?.textContent)})`);
  await React.act(async () => { host.querySelector<HTMLButtonElement>("[data-turn-always-toggle]")!.click(); });
  const prefixBdi = host.querySelector("[data-turn-always-prefix] bdi");
  assert(prefixBdi !== null && prefixBdi.textContent === "git status", `TurnBar prefix strips bidi/control and wraps <bdi> (got ${JSON.stringify(prefixBdi?.textContent)})`);

  await render(
    React.createElement(PermissionCard, {
      card: {
        id: "perm-noncompact",
        argv: dirty,
        cwd: "/tmp/\u202Edir\x07",
        reason: "ok\nline2\u202E",
        risk: "high",
        prefix: null,
        status: "pending",
        decisionLabel: "",
      },
      readOnly: true,
      onDone: () => undefined,
      compact: false,
    }),
  );
  const bdiTexts = Array.from(host.querySelectorAll("bdi")).map((n) => n.textContent ?? "");
  assert(bdiTexts.includes(cleanExpected), "non-compact PermissionCard strips control/bidi from argv inside <bdi>");
  assert(bdiTexts.includes("/tmp/dir"), "non-compact PermissionCard strips control/bidi from cwd inside <bdi>");
  assert(bdiTexts.some((t) => t === "ok\nline2"), "ReasonBlock keeps newline and strips bidi inside <bdi>");

  const { formatRuleCommand } = require("../src/features/home/permissionFormat");
  assert(
    formatRuleCommand({ scope: "prefix", argv: ["git\u202E", "status foo"], tail: ["--hard\u2066"] }) === 'git "status foo" --hard',
    "formatRuleCommand strips bidi chars and JSON-quotes spaced args",
  );
  assert(
    formatRuleCommand({ scope: "exact", argv: ["echo", "a\x07\u202Eb"], tail: [] }) === 'echo "a\\u0007b"',
    "formatRuleCommand escapes C0 controls and strips bidi chars",
  );

  // ---- 9. CAD-2319: ThreadPermission skips /api/master/permissions when disabled ----
  calls.length = 0;
  await render(
    React.createElement(
      PermissionEnabledContext.Provider,
      { value: false },
      React.createElement(ThreadPermission, {
        text: "Permission requested (perm-disabled, risk high): rm -rf /tmp",
        readOnly: true,
      }),
    ),
  );
  assert(
    !calls.some((c) => c.path === "/api/master/permissions"),
    "ThreadPermission under PermissionEnabledContext=false makes zero /api/master/permissions calls",
  );
  assert(host.querySelector("[data-permission-thread='perm-disabled']") !== null, "ThreadPermission still renders message text when disabled");

  // ---- 10. CAD-616: worker attribution, idle turnBarState approval, and agent-scoped revoke ----
  assert(
    rejectReplyPrompt("git status", "worker-1") === "For worker-1, instead of `git status`: ",
    "worker rejectReplyPrompt names worker alias",
  );
  assert(
    rejectReplyPrompt("echo `hi`", "worker-1") === 'For worker-1, instead of "echo `hi`": ',
    "worker rejectReplyPrompt falls back to quotes when command contains backtick",
  );
  assert(
    turnBarState(
      { master: null, entries: [], pendingMessages: [], awaitingApproval: true, reconnecting: false },
      "conv-home",
    ).kind === "approval",
    "idle turnBarState returns approval when awaitingApproval is true",
  );

  await render(
    React.createElement(TurnBar, {
      state: { kind: "approval" },
      reconnecting: false,
      actions: { onReview: () => undefined },
      permission: {
        request: { ...makeReq("perm-w1", "ls -la", null), requested_by: "worker-1" },
        readOnly: false,
        onRejectAndReply: () => undefined,
        onFocusComposer: () => undefined,
      },
    }),
  );
  assert(
    host.querySelector("[data-turn-agent='worker-1']")?.textContent?.trim() === "worker-1",
    "TurnBar renders worker attribution chip",
  );

  await render(
    React.createElement(PermissionCard, {
      card: cardFromRequest({ ...makeReq("perm-w2", "git diff", null), requested_by: "worker-1" }),
      readOnly: false,
      onDone: () => undefined,
    }),
  );
  assert(
    host.querySelector("[data-permission-agent='worker-1']")?.textContent?.trim() === "worker-1",
    "PermissionCard renders worker attribution chip",
  );
  assert(
    (host.querySelector("[data-permission-reason]")?.textContent ?? "").includes("Tool request:"),
    "PermissionCard uses Tool request label for worker requests",
  );

  savedRules.push({
    id: "rule-w1",
    effect: "allow",
    scope: "exact",
    argv: ["ls", "-la"],
    tail: [],
    cwd: "/repo",
    agent: "worker-1",
    by: "operator",
    at: 1700000200,
  } as (typeof savedRules)[number] & { agent: string });
  permGetMode = "ok";
  permPostStatus = 200;
  calls.length = 0;
  await render(React.createElement(PermissionsSlashCard, { readOnly: false }));
  assert(
    host.querySelector("[data-rule-agent='worker-1']")?.textContent?.trim() === "worker-1",
    "PermissionsSlashCard renders worker badge on worker rule",
  );
  const w1Revoke = host.querySelector<HTMLButtonElement>("[data-permission-revoke='rule-w1']");
  assert(w1Revoke !== null, "Worker rule revoke button present");
  await React.act(async () => { w1Revoke.click(); });
  await settle();
  const w1Confirm = host.querySelector<HTMLButtonElement>("[data-permission-confirm-revoke='rule-w1']");
  assert(w1Confirm !== null, "Worker rule confirm revoke button present");
  await React.act(async () => { w1Confirm.click(); });
  await settle();
  assert(
    calls.some(
      (c) =>
        c.method === "POST"
        && c.path === "/api/master/permissions/rules/rule-w1/revoke"
        && JSON.stringify(c.body) === JSON.stringify({ agent: "worker-1" }),
    ),
    "PermissionsSlashCard passes agent in revoke POST body",
  );

  await React.act(async () => { root.unmount(); });
  await win.happyDOM.close();
  console.log("in-chat permissions checks passed");
}

void main();
