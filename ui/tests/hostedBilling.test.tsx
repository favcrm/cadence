export {};
/**
 * CAD-2726: Hosted Settings > Billing security URL guards and end-to-end
 * component acceptance states.
 */
import {
  assertCondition as assert,
  assertEqual as equal,
  createEventSignal,
  installHostedDom,
  waitForDom,
} from "./hostedDom.fixture";

declare function require<T>(name: string): T;

const win = installHostedDom("https://acme.cadencecloud.app/settings/account");

const React = require<typeof import("react")>("react");
const { createRoot } = require<typeof import("react-dom/client")>("react-dom/client");
const { LocaleProvider } = require<typeof import("../src/lib/locale")>("../src/lib/locale");
const PlatformAccount = require<typeof import("../src/features/settings/PlatformAccount")>(
  "../src/features/settings/PlatformAccount",
).default;
const display = require<typeof import("../src/features/settings/accountDisplay")>(
  "../src/features/settings/accountDisplay",
);

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
  // Security boundary & fail-closed payload checks
  assert(display.parseHostedBilling(syntheticSnapshot) !== null, "valid snapshot parses cleanly");
  assert(display.parseHostedBilling({ role: "owner" }) === null, "malformed snapshot fails closed");
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
  let redirectSignal = createEventSignal<string>();
  let reportedSnapshot: unknown = null;
  const host = document.createElement("div");
  document.body.append(host);

  const mountView = async (waitForReady: () => Promise<void>) => {
    redirectSignal = createEventSignal<string>();
    const root = createRoot(host);
    await React.act(async () => {
      root.render(
        React.createElement(LocaleProvider, {
          children: React.createElement(
            PlatformAccount as React.ComponentType<{
              hosted?: boolean;
              onBillingSnapshot?: (snapshot: unknown) => void;
              onRedirect?: (url: string) => void;
            }>,
            {
              hosted: true,
              onBillingSnapshot: (snap: unknown) => {
                reportedSnapshot = snap;
              },
              onRedirect: (url: string) => {
                redirects.push(url);
                redirectSignal.emit(url);
              },
            },
          ),
        }),
      );
    });
    await waitForReady();
    return async () => {
      await React.act(async () => root.unmount());
    };
  };

  // 1. Owner view with split balance, monthly/yearly toggle, packs, subscribe & topup CSRF headers
  let unmount = await mountView(() =>
    waitForDom(
      React.act,
      host,
      () => Boolean(host.querySelector('[data-testid="billing-total-credits"]')),
      "owner billing summary",
    ),
  );
  assert(reportedSnapshot !== null, "onBillingSnapshot reports parsed snapshot");
  assert(host.textContent?.includes("2,000"), "total credits displayed");
  assert(host.textContent?.includes("500") && host.textContent?.includes("1,500"), "split credits displayed");
  assert(host.textContent?.includes("Free trial"), "active trial badge visible");
  assert(host.textContent?.includes("$17"), "monthly USD price shown by default");
  assert(!host.textContent?.includes("$163"), "yearly price hidden until toggled");

  const yearlyBtn = Array.from(host.querySelectorAll("button")).find(
    (b) => b.textContent === "Yearly billing",
  ) as HTMLButtonElement;
  assert(yearlyBtn, "yearly toggle button present");
  await React.act(async () => yearlyBtn.click());
  assert(host.textContent?.includes("$163"), "yearly USD price shown after toggle");

  const choosePlanBtn = host.querySelector('[data-plan-id="synth_basic_yr"] button') as HTMLButtonElement;
  await React.act(async () => choosePlanBtn.click());
  const firstRedirect = await redirectSignal.wait(React.act, "subscribe redirect");
  const subReq = requests.find((r) => r.path === "/api/account/billing/subscribe");
  assert(subReq, "subscribe endpoint called");
  equal(subReq.headers.get("x-cadence-board"), "1", "subscribe sends X-Cadence-Board");
  equal(subReq.headers.get("content-type"), "application/json", "subscribe sends application/json");
  equal(subReq.body, JSON.stringify({ planId: "synth_basic_yr" }), "subscribe sends selected planId");
  equal(firstRedirect, "https://checkout.stripe.com/c/pay/cs_synth_1", "redirects to Stripe Checkout");

  // Unsafe redirect URL is refused
  nextPostPayload = { url: "https://evil.example.test/steal" };
  const buyPackBtn = host.querySelector('[data-pack-id="pack_alpha"] button') as HTMLButtonElement;
  await React.act(async () => buyPackBtn.click());
  await waitForDom(
    React.act,
    host,
    () => Boolean(host.textContent?.includes("redirect link was invalid")),
    "unsafe redirect inline error",
  );
  equal(redirects.length, 1, "unsafe topup URL does not redirect");
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
  unmount = await mountView(() =>
    waitForDom(
      React.act,
      host,
      () => Boolean(host.textContent?.includes("訂閱方案")),
      "zh-TW billing heading and plans",
    ),
  );
  assert(host.textContent?.includes("帳單與方案"), "zh-TW billing heading rendered");
  assert(host.textContent?.includes("訂閱方案"), "zh-TW Subscription plans label rendered without Plans collision");
  assert(
    host.textContent?.includes("月繳") && host.textContent?.includes("年繳"),
    "zh-TW Monthly/Yearly billing labels rendered without collision",
  );
  assert(!host.textContent?.includes("計畫") && !host.textContent?.includes("按月"), "zh-TW avoids collided native task strings");
  assert(host.textContent?.includes("≈ HK$"), "zh-TW secondary HKD reference shown");
  assert(host.textContent?.includes("以美元 (USD) 經 Stripe 結算，港幣金額僅供參考。"), "zh-TW disclosure shown");
  const portalBtn = Array.from(host.querySelectorAll("button")).find(
    (b) => b.textContent === "管理帳單",
  ) as HTMLButtonElement;
  assert(portalBtn, "active Stripe subscription shows Manage billing button");
  nextPostPayload = { url: "https://billing.stripe.com/p/session/bps_synth_1" };
  await React.act(async () => portalBtn.click());
  const secondRedirect = await redirectSignal.wait(React.act, "portal redirect");
  equal(secondRedirect, "https://billing.stripe.com/p/session/bps_synth_1", "portal opens billing.stripe.com");
  await unmount();
  win.localStorage.removeItem("cadence-locale");

  // 3. Return banner + URL scrub
  win.history.replaceState({}, "", "https://acme.cadencecloud.app/settings/account?subscription=success");
  nextBillingPayload = syntheticSnapshot;
  unmount = await mountView(() =>
    waitForDom(
      React.act,
      host,
      () => Boolean(host.textContent?.includes("Subscription updated")),
      "subscription return banner",
    ),
  );
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
  unmount = await mountView(() =>
    waitForDom(
      React.act,
      host,
      () => Boolean(host.querySelector('[data-testid="billing-member-readonly"]')),
      "member read-only notice",
    ),
  );
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
  unmount = await mountView(() =>
    waitForDom(
      React.act,
      host,
      () => Boolean(host.querySelector('[data-testid="billing-iap-notice"]')),
      "IAP notice",
    ),
  );
  assert(
    host.querySelector('[data-testid="billing-iap-notice"]')?.textContent?.includes("App Store"),
    "IAP store notice shown",
  );
  const manageBtns = Array.from(host.querySelectorAll("button")).filter((b) => b.textContent === "Manage billing");
  equal(manageBtns.length, 0, "IAP subscription hides Stripe portal button");
  await unmount();

  // 6. 404 graceful degradation
  nextBillingStatus = 404;
  nextBillingPayload = { error: { code: "not_found", message: "Not found" } };
  unmount = await mountView(() =>
    waitForDom(
      React.act,
      host,
      () => Boolean(host.textContent?.includes("Billing is not enabled on this board yet.")),
      "404 unavailable notice",
    ),
  );

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
