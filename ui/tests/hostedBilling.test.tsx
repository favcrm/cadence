export {};
/**
 * CAD-2726: Hosted Settings > Billing display helpers and component states.
 */
declare function require(name: string): any;

const { Window } = require("happy-dom");
const win = new Window({ url: "https://acme.cadencecloud.app/settings/account" });
for (const name of [
  "window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement",
  "HTMLButtonElement", "SVGElement", "navigator", "MutationObserver",
  "Event", "MouseEvent", "KeyboardEvent", "location", "history",
  "localStorage", "sessionStorage",
]) {
  Object.defineProperty(globalThis, name, {
    value: name === "window" ? win : win[name],
    configurable: true,
    writable: true,
  });
}
for (const name of ["addEventListener", "removeEventListener"] as const) {
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });
}
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
const PlatformAccount = (require("../src/features/settings/PlatformAccount") as typeof import("../src/features/settings/PlatformAccount")).default;
const display = require("../src/features/settings/accountDisplay") as typeof import("../src/features/settings/accountDisplay");

function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(`hostedBilling test failed: ${message}`);
}

function equal(actual: unknown, expected: unknown, message: string): void {
  if (actual !== expected) {
    throw new Error(`${message}: expected ${String(expected)}, got ${String(actual)}`);
  }
}

async function flush(): Promise<void> {
  await React.act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

// Pure formatter & security helpers (using synthetic pricing values)
equal(display.formatCredits("0.000000"), "0", "zero credits");
equal(display.formatCredits("0.000050"), "< 0.01", "sub-hundredth credit");
equal(display.formatCredits("20.000000"), "2,000", "whole credits at 100 per USD");
equal(display.formatCredits("12.345000"), "1,234.5", "fractional credits");
equal(display.formatSignedCredits("+15.000000"), "+1,500", "positive signed credits");
equal(display.formatSignedCredits("-1.500000"), "-150", "negative signed credits");
equal(display.formatSignedCredits("0.000000"), "0", "zero signed credits");
equal(display.formatUsdFromScaled("17.000000"), "$17", "whole USD amount");
equal(display.formatUsdFromScaled("17.500000"), "$17.50", "fractional USD amount");
equal(display.formatUsdFromMinor(900), "$9", "minor cents USD");
equal(display.formatApproxHkdFromScaled("10.000000"), "≈ HK$78", "approx HKD from scale-6");
equal(display.formatApproxHkdFromMinor(1000), "≈ HK$78", "approx HKD from minor cents");
{
  const split = display.creditSplitPercentages("5.000000", "15.000000");
  equal(split.includedPct, 25, "included split percent");
  equal(split.refillPct, 75, "refill split percent");
}
equal(
  display.safeStripeCheckoutUrl("https://checkout.stripe.com/c/pay/cs_test_1"),
  "https://checkout.stripe.com/c/pay/cs_test_1",
  "accepts Stripe checkout URL",
);
equal(
  display.safeStripeCheckoutUrl("https://billing.stripe.com/p/session/bps_1"),
  "https://billing.stripe.com/p/session/bps_1",
  "accepts Stripe billing portal URL",
);
for (const bad of [
  "javascript:alert(1)",
  "http://checkout.stripe.com/c/pay/cs_1",
  "https://user@checkout.stripe.com/c/pay/cs_1",
  "https://evil.example.test/checkout",
]) {
  equal(display.safeStripeCheckoutUrl(bad), null, `rejects unsafe checkout URL ${bad}`);
}
equal(
  display.safeWorkspaceUrl("https://beta.cadencecloud.app"),
  "https://beta.cadencecloud.app/",
  "accepts cadencecloud workspace URL",
);
equal(
  display.safeWorkspaceUrl("https://app-v2.agenticos.hk/register?mode=new"),
  "https://app-v2.agenticos.hk/register?mode=new",
  "accepts web register URL",
);
for (const badWs of [
  "javascript:alert(1)",
  "http://acme.cadencecloud.app",
  "https://user@acme.cadencecloud.app",
  "https://acme.cadencecloud.app.evil.test",
]) {
  equal(display.safeWorkspaceUrl(badWs), null, `rejects unsafe workspace URL ${badWs}`);
}
equal(display.readBillingReturnParams("?subscription=success")?.kind, "subscription", "parses subscription return");
equal(display.readBillingReturnParams("?topup=cancel")?.status, "cancel", "parses topup cancel");

const syntheticSnapshot = {
  role: "owner" as const,
  canManage: true,
  workspace: {
    id: "ws_synth",
    name: "Northstar Synthetic",
    nameZh: "北極星合成工作區",
    slug: "northstar",
  },
  account: {
    company: { name: "Northstar Synthetic" },
    balance: {
      amount: "20.000000",
      currency: "USD" as const,
      low: false,
      zero: false,
      includedAmount: "5.000000",
      refillAmount: "15.000000",
    },
    plan: {
      name: "Synth Pro",
      status: "inactive" as const,
    },
    links: { account: "https://northstar.cadencecloud.app/settings/account" },
    packs: [
      {
        id: "pack_alpha",
        label: "Alpha Booster",
        chargeMinor: 900,
        credit: "900.000000",
        available: true,
      },
      {
        id: "pack_disabled",
        label: "Offline Pack",
        chargeMinor: 1900,
        credit: "1900.000000",
        available: false,
      },
    ],
    trial: {
      status: "active" as const,
      startedAt: "2026-11-20T00:00:00Z",
      deadlineAt: "2026-11-27T00:00:00Z",
      credit: "5.000000",
      tokenCap: 100000,
      tokensUsed: 12000,
    },
  },
  offer: {
    asOf: "2026-11-23T00:00:00Z",
    plan: null,
    plans: [
      {
        id: "synth_basic_mo",
        label: "Synth Basic",
        amount: "17.000000",
        currency: "USD" as const,
        period: "month" as const,
        includedCredit: "17.000000",
        creditSlices: 1,
        grantsEnabled: true,
      },
      {
        id: "synth_basic_yr",
        label: "Synth Basic",
        amount: "163.000000",
        currency: "USD" as const,
        period: "year" as const,
        includedCredit: "17.000000",
        creditSlices: 12,
        grantsEnabled: true,
      },
    ],
    trial: {
      state: "active" as const,
      eligibility: "used" as const,
      eligibilityReason: "trial_row",
      startedAt: "2026-11-20T00:00:00Z",
      deadlineAt: "2026-11-27T00:00:00Z",
      credit: "5.000000",
      tokenCap: 100000,
      tokensUsed: 12000,
      terms: null,
    },
    subscription: { state: "none" as const },
    checkout: {
      id: null,
      state: "none" as const,
      sessionId: null,
      url: null,
      expiresAt: null,
      available: true,
      blocked: null,
    },
  },
  receivable: "0.000000",
  usage: {
    rows: [
      {
        date: "2026-11-22T10:00:00Z",
        description: "Wallet credit",
        amount: "15.000000",
        kind: "credit" as const,
      },
      {
        date: "2026-11-22T11:00:00Z",
        description: "Model usage",
        amount: "-1.500000",
        kind: "charge" as const,
      },
    ],
    cursor: null,
  },
  manageUrl: "https://app-v2.agenticos.hk/account?company=northstar",
};

async function run(): Promise<void> {
  assert(display.parseHostedBilling(syntheticSnapshot) !== null, "valid snapshot parses cleanly");
  assert(display.parseHostedBilling({ role: "owner" }) === null, "malformed snapshot fails closed");

  const requests: Array<{ path: string; headers: Headers; body: string }> = [];
  let nextBillingStatus = 200;
  let nextBillingPayload: unknown = syntheticSnapshot;
  let nextPostPayload: unknown = { url: "https://checkout.stripe.com/c/pay/cs_synth_1" };

  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const path = String(input);
    requests.push({
      path,
      headers: new Headers(init?.headers),
      body: typeof init?.body === "string" ? init.body : "",
    });
    if (path === "/api/account/billing") {
      return new Response(JSON.stringify(nextBillingPayload), {
        status: nextBillingStatus,
        headers: { "Content-Type": "application/json" },
      });
    }
    return new Response(JSON.stringify(nextPostPayload), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  }) as typeof fetch;

  const redirects: string[] = [];
  let reportedSnapshot: unknown = null;
  const host = document.createElement("div");
  document.body.append(host);
  const mountView = async () => {
    const root = createRoot(host);
    await React.act(async () => {
      root.render(
        React.createElement(LocaleProvider, {
          children: React.createElement(PlatformAccount as React.ComponentType<{
            hosted?: boolean;
            onBillingSnapshot?: (snapshot: unknown) => void;
            onRedirect?: (url: string) => void;
          }>, {
            hosted: true,
            onBillingSnapshot: (snap: unknown) => {
              reportedSnapshot = snap;
            },
            onRedirect: (url: string) => redirects.push(url),
          }),
        }),
      );
    });
    await flush();
    return async () => {
      await React.act(async () => root.unmount());
    };
  };

  // 1. Owner view with split balance, monthly/yearly toggle, packs, subscribe & topup CSRF headers
  let unmount = await mountView();
  assert(reportedSnapshot !== null, "onBillingSnapshot reports parsed snapshot");
  assert(host.textContent?.includes("2,000"), "total credits displayed");
  assert(host.textContent?.includes("500") && host.textContent?.includes("1,500"), "split credits displayed");
  assert(host.textContent?.includes("Free trial"), "active trial badge visible");
  assert(host.textContent?.includes("$17"), "monthly USD price shown by default");
  assert(!host.textContent?.includes("$163"), "yearly price hidden until toggled");

  const yearlyBtn = Array.from(host.querySelectorAll("button")).find((b: any) => b.textContent === "Yearly billing") as HTMLButtonElement;
  assert(yearlyBtn, "yearly toggle button present");
  await React.act(async () => yearlyBtn.click());
  assert(host.textContent?.includes("$163"), "yearly USD price shown after toggle");

  const choosePlanBtn = host.querySelector('[data-plan-id="synth_basic_yr"] button') as HTMLButtonElement;
  await React.act(async () => choosePlanBtn.click());
  await flush();
  const subReq = requests.find((r) => r.path === "/api/account/billing/subscribe");
  assert(subReq, "subscribe endpoint called");
  equal(subReq.headers.get("x-cadence-board"), "1", "subscribe sends X-Cadence-Board");
  equal(subReq.headers.get("content-type"), "application/json", "subscribe sends application/json");
  equal(subReq.body, JSON.stringify({ planId: "synth_basic_yr" }), "subscribe sends selected planId");
  equal(redirects[0], "https://checkout.stripe.com/c/pay/cs_synth_1", "redirects to Stripe Checkout");

  // Unsafe redirect URL is refused
  nextPostPayload = { url: "https://evil.example.test/steal" };
  const buyPackBtn = host.querySelector('[data-pack-id="pack_alpha"] button') as HTMLButtonElement;
  await React.act(async () => buyPackBtn.click());
  await flush();
  equal(redirects.length, 1, "unsafe topup URL does not redirect");
  assert(host.textContent?.includes("redirect link was invalid"), "unsafe URL surfaces inline error");
  await unmount();

  // 2. zh-TW locale shows approx HKD references + formal USD Stripe settlement disclosure
  win.localStorage.setItem("cadence-locale", "zh-TW");
  nextBillingPayload = {
    ...syntheticSnapshot,
    account: {
      ...syntheticSnapshot.account,
      plan: {
        name: "Synth Basic",
        status: "active" as const,
        provider: "stripe" as const,
        planId: "synth_basic_mo",
      },
    },
  };
  unmount = await mountView();
  assert(host.textContent?.includes("帳單與方案"), "zh-TW billing heading rendered");
  assert(host.textContent?.includes("訂閱方案"), "zh-TW Subscription plans label rendered without Plans collision");
  assert(host.textContent?.includes("月繳") && host.textContent?.includes("年繳"), "zh-TW Monthly/Yearly billing labels rendered without collision");
  assert(!host.textContent?.includes("計畫") && !host.textContent?.includes("按月"), "zh-TW avoids collided native task strings");
  assert(host.textContent?.includes("≈ HK$"), "zh-TW secondary HKD reference shown");
  assert(host.textContent?.includes("以美元 (USD) 經 Stripe 結算，港幣金額僅供參考。"), "zh-TW disclosure shown");
  const portalBtn = Array.from(host.querySelectorAll("button")).find((b: any) => b.textContent === "管理帳單") as HTMLButtonElement;
  assert(portalBtn, "active Stripe subscription shows Manage billing button");
  nextPostPayload = { url: "https://billing.stripe.com/p/session/bps_synth_1" };
  await React.act(async () => portalBtn.click());
  await flush();
  equal(redirects[1], "https://billing.stripe.com/p/session/bps_synth_1", "portal opens billing.stripe.com");
  await unmount();
  win.localStorage.removeItem("cadence-locale");

  // 3. Return banner + URL scrub
  win.history.replaceState(null, "", "https://acme.cadencecloud.app/settings/account?subscription=success");
  nextBillingPayload = syntheticSnapshot;
  unmount = await mountView();
  assert(host.textContent?.includes("Subscription updated"), "return banner visible");
  equal(win.location.search, "", "subscription return query param scrubbed from URL");
  await unmount();

  // 4. Member read-only view hides balances, plans & packs
  nextBillingPayload = {
    role: "member",
    canManage: false,
    workspace: syntheticSnapshot.workspace,
    account: null,
    offer: null,
    receivable: null,
    usage: { rows: [], cursor: null },
    manageUrl: null,
  };
  unmount = await mountView();
  assert(host.querySelector('[data-testid="billing-member-readonly"]'), "member read-only notice rendered");
  assert(!host.querySelector('[data-testid="billing-total-credits"]'), "member hides credit balance");
  assert(!host.querySelector("#billing-packs"), "member hides refill packs");
  await unmount();

  // 5. Apple IAP managed notice disables Stripe portal
  nextBillingPayload = {
    ...syntheticSnapshot,
    account: {
      ...syntheticSnapshot.account,
      plan: { name: "Synth Mobile", status: "active", provider: "apple", planId: "synth_basic_mo" },
    },
  };
  unmount = await mountView();
  assert(host.querySelector('[data-testid="billing-iap-notice"]')?.textContent?.includes("App Store"), "IAP store notice shown");
  const manageBtns = Array.from(host.querySelectorAll("button")).filter((b: any) => b.textContent === "Manage billing");
  equal(manageBtns.length, 0, "IAP subscription hides Stripe portal button");
  await unmount();

  // 6. 404 graceful degradation
  nextBillingStatus = 404;
  nextBillingPayload = { error: { code: "not_found", message: "Not found" } };
  unmount = await mountView();
  assert(host.textContent?.includes("Billing is not enabled on this board yet."), "404 shows calm unavailable message");

  await unmount();
  host.remove();
  console.log("hostedBilling checks passed");
}

declare const process: { exit: (code?: number) => void };
run().then(
  () => process.exit(0),
  (err) => {
    console.error(err);
    process.exit(1);
  },
);
