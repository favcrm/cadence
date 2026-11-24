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
const recovery = require<{
  subscription: {
    ready: { kind: "subscription"; state: "ready"; attemptId: string; url: string };
    pending: { kind: "subscription"; state: "pending"; attemptId: string };
  };
  topup: { blocked: { kind: "topup"; state: "blocked"; attemptId: string } };
}>(`${process.cwd()}/tests/fixtures/board-account-recovery.v1.json`);

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
  let nextPostPayload: unknown = recovery.subscription.ready;
  let attemptState = "pending";

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
    if (path.startsWith("/api/account/billing?cursor=")) {
      const base =
        typeof nextBillingPayload === "object" && nextBillingPayload !== null
          ? (nextBillingPayload as typeof syntheticSnapshot)
          : syntheticSnapshot;
      return new Response(
        JSON.stringify({
          ...base,
          usage: {
            rows: [
              {
                date: "2026-11-10T09:15:00Z",
                description: "Older run",
                amount: "-0.850000",
                kind: "charge" as const,
                model: "Claude Haiku",
                source: "monthly" as const,
                usagePercent: 5.0,
              },
            ],
            cursor: null,
          },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    if (path.startsWith("/api/account/billing/attempt?")) {
      const query = new URL(path, "https://acme.cadencecloud.app").searchParams;
      if (query.get("kind") !== "subscription" || query.get("attemptId") !== recovery.subscription.ready.attemptId) {
        return new Response(null, { status: 404 });
      }
      return Response.json({ ...recovery.subscription.pending, state: attemptState });
    }
    return new Response(JSON.stringify(nextPostPayload), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  }) as typeof fetch;

  // Percentage usage & sidebar helper unit assertions
  const sampleRichUsage = {
    mode: "monthly_standby" as const,
    cycle: { usedPercent: 38, remainingPercent: 62, resetsAt: "2026-12-01T00:00:00Z" },
    extra: { hasReserve: true, poolRemainingPercent: 100, poolUsedPercent: 0, status: "standby" as const },
    denominatorBasis: "monthly_plan" as const,
  };
  assert(
    display.parseHostedBilling({
      ...syntheticSnapshot,
      account: { ...syntheticSnapshot.account, usageSummary: sampleRichUsage },
      usage: {
        ...syntheticSnapshot.usage,
        dailyUsage: [
          {
            date: "2026-11-22",
            totalPercent: 8.8,
            monthlyPercent: 8.8,
            extraPercent: 0,
            runCount: 3,
            models: ["Claude Sonnet"],
            source: "monthly" as const,
          },
        ],
      },
    }) !== null,
    "parses snapshot with usageSummary and dailyUsage",
  );
  equal(
    display.planCapacityLabel(syntheticSnapshot.offer.plans, syntheticSnapshot.offer.plans[0]),
    "1×",
    "base monthly plan shows 1×",
  );
  equal(
    display.packCapacityLabel(
      [{ id: "p50", label: "Half", chargeMinor: 500, credit: "8.500000", available: true }],
      syntheticSnapshot.offer.plans,
      { id: "p50", label: "Half", chargeMinor: 500, credit: "8.500000", available: true },
    ),
    "+50%",
    "pack smaller than monthly plan formats as +50%",
  );
  equal(display.formatUsagePercent(0.04), "<0.1%", "tiny share formats as <0.1%");
  equal(display.formatUsagePercent(1.44), "1.4%", "standard share formats with 1 decimal");

  const sidebarStandby = display.sidebarBillingStatus(
    { ...syntheticSnapshot.account, plan: { name: "Synth Pro", status: "active", planId: "synth_basic_mo" }, usageSummary: sampleRichUsage },
    syntheticSnapshot.offer,
  );
  equal(sidebarStandby.metricKind, "used", "standby sidebar tracks monthly % used");
  equal(sidebarStandby.percent, 38, "standby sidebar reports 38% used");
  equal(sidebarStandby.tone, "ok", "38% used sidebar tone is ok");

  const sidebarOverflow = display.sidebarBillingStatus(
    {
      ...syntheticSnapshot.account,
      plan: { name: "Synth Pro", status: "active", planId: "synth_basic_mo" },
      usageSummary: {
        mode: "overflow",
        cycle: { usedPercent: 100, remainingPercent: 0, resetsAt: "2026-12-01T00:00:00Z" },
        extra: { hasReserve: true, poolRemainingPercent: 79, poolUsedPercent: 21, status: "active" },
        denominatorBasis: "monthly_plan",
      },
    },
    syntheticSnapshot.offer,
  );
  equal(sidebarOverflow.metricKind, "extra_left", "overflow sidebar switches to extra remaining %");
  equal(sidebarOverflow.percent, 79, "overflow sidebar shows 79% left");
  equal(sidebarOverflow.tone, "info", "overflow sidebar tone is info");

  const sidebarDepleted = display.sidebarBillingStatus(
    {
      ...syntheticSnapshot.account,
      plan: { name: "Synth Pro", status: "active", planId: "synth_basic_mo" },
      usageSummary: {
        mode: "depleted",
        cycle: { usedPercent: 100, remainingPercent: 0, resetsAt: "2026-12-01T00:00:00Z" },
        extra: { hasReserve: false, poolRemainingPercent: 0, poolUsedPercent: 0, status: "depleted" },
        denominatorBasis: "monthly_plan",
      },
    },
    syntheticSnapshot.offer,
  );
  equal(sidebarDepleted.metricKind, "depleted", "depleted sidebar tracks depleted state");
  equal(sidebarDepleted.tone, "fail", "depleted sidebar tone is fail");

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
              sessionScope?: string | null;
              onBillingSnapshot?: (snapshot: unknown) => void;
              onRedirect?: (url: string) => void;
            }>,
            {
              hosted: true,
              sessionScope: "usr_contract_owner",
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

  // 1. Owner view with percentage usage summary, monthly/yearly toggle, multiplier badges, subscribe & topup CSRF headers
  let unmount = await mountView(() =>
    waitForDom(
      React.act,
      host,
      () => Boolean(host.querySelector('[data-testid="billing-usage-summary"]')),
      "owner billing summary",
    ),
  );
  assert(reportedSnapshot !== null, "onBillingSnapshot reports parsed snapshot");
  equal(
    host.querySelector('[data-testid="billing-usage-percent"]')?.textContent?.trim(),
    "12%",
    "trial percentage used displayed",
  );
  assert(host.textContent?.includes("88% remaining"), "remaining percentage displayed");
  const renderedText = host.textContent ?? "";
  assert(
    !/\b(2,000|1,500|500)\b/.test(renderedText) && !/\bcredits\b/i.test(renderedText),
    "no raw credit counts rendered",
  );
  assert(host.textContent?.includes("Free trial"), "active trial badge visible");
  assert(Boolean(host.querySelector('[data-testid="billing-extra-card"]')), "extra usage hero card rendered");
  assert(!host.querySelector("[data-plan-id]") && !host.querySelector("#billing-packs"), "plans and extra packs stay tucked in modals on initial load");

  const openPlansBtn = host.querySelector("#billing-open-plans") as HTMLButtonElement;
  assert(openPlansBtn, "Choose plan / Upgrade button present");
  await React.act(async () => openPlansBtn.click());
  const plansModal = host.querySelector('[data-testid="billing-plans-modal"]');
  assert(Boolean(plansModal), "plans modal opens on click");
  assert(plansModal?.textContent?.includes("1× monthly usage capacity"), "relative plan capacity multiplier displayed");
  assert(plansModal?.textContent?.includes("$17"), "monthly USD price shown by default");
  assert(!plansModal?.textContent?.includes("$163"), "yearly price hidden until toggled");

  const yearlyBtn = Array.from(plansModal?.querySelectorAll("button") ?? []).find(
    (b) => b.textContent === "Yearly billing",
  ) as HTMLButtonElement;
  assert(yearlyBtn, "yearly toggle button present");
  await React.act(async () => yearlyBtn.click());
  assert(host.querySelector('[data-testid="billing-plans-modal"]')?.textContent?.includes("$163"), "yearly USD price shown after toggle");

  const choosePlanBtn = host.querySelector('[data-plan-id="synth_basic_yr"] button') as HTMLButtonElement;
  await React.act(async () => choosePlanBtn.click());
  const firstRedirect = await redirectSignal.wait(React.act, "subscribe redirect");
  const subReq = requests.find((r) => r.path === "/api/account/billing/checkout");
  assert(subReq, "subscribe endpoint called");
  equal(subReq.headers.get("x-cadence-board"), "1", "subscribe sends X-Cadence-Board");
  equal(subReq.headers.get("content-type"), "application/json", "subscribe sends application/json");
  const selected = JSON.parse(subReq.body) as { kind: string; planId: string };
  equal(selected.kind, "subscription", "checkout sends subscription kind");
  equal(selected.planId, "synth_basic_yr", "checkout sends selected planId");
  equal(firstRedirect, recovery.subscription.ready.url, "redirects to exact fixture Stripe Checkout");

  await React.act(async () => {
    win.document.dispatchEvent(new win.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  });
  assert(!host.querySelector('[data-testid="billing-plans-modal"]'), "plans modal closes on Escape");
  await unmount();

  win.history.replaceState({}, "", "/settings/account?subscription=success");
  unmount = await mountView(() => waitForDom(
    React.act,
    host,
    () => Boolean(host.textContent?.includes("Confirming payment")),
    "pending Stripe return",
  ));
  assert(!host.textContent?.includes("Checkout confirmed."), "a return flag alone is not payment proof");
  assert(requests.some((r) => r.path.includes(`attemptId=${recovery.subscription.ready.attemptId}`)), "only the stored attempt is checked");
  attemptState = "confirmed";
  const refresh = Array.from(host.querySelectorAll("button")).find((b) => b.textContent?.trim() === "Refresh");
  assert(refresh, "pending return has a refresh");
  await React.act(async () => refresh.click());
  await waitForDom(React.act, host, () => Boolean(host.textContent?.includes("Checkout confirmed.")), "exact attempt confirmation");
  equal(requests.filter((r) => r.path === "/api/account/billing/checkout").length, 1, "refresh does not retry a purchase");
  await unmount();
  win.history.replaceState({}, "", "/settings/account");

  win.sessionStorage.clear();
  unmount = await mountView(() => waitForDom(
    React.act, host, () => Boolean(host.querySelector("#billing-open-extra")), "fresh owner extra usage",
  ));

  // Unsafe redirect URL is refused inside Extra usage modal
  const openExtraBtn = host.querySelector("#billing-open-extra") as HTMLButtonElement;
  assert(openExtraBtn, "Add extra usage button present");
  await React.act(async () => openExtraBtn.click());
  assert(Boolean(host.querySelector('[data-testid="billing-extra-modal"] #billing-packs')), "extra usage modal opens with refill packs");
  nextPostPayload = { ...recovery.topup.blocked, state: "ready", url: "https://evil.example.test/steal" };
  const buyPackBtn = host.querySelector('[data-pack-id="pack_alpha"] button') as HTMLButtonElement;
  await React.act(async () => buyPackBtn.click());
  await waitForDom(
    React.act,
    host,
    () => Boolean(host.textContent?.includes("Checkout status is unknown") || host.textContent?.includes("redirect link was invalid")),
    "unsafe checkout URL preserves uncertainty instead of navigating",
  );
  equal(redirects.length, 1, "unsafe topup URL does not redirect");
  const closeExtraBtn = host.querySelector('[data-testid="billing-extra-modal"] button[aria-label="Close"]') as HTMLButtonElement;
  await React.act(async () => closeExtraBtn.click());
  assert(!host.querySelector('[data-testid="billing-extra-modal"]'), "extra modal closes on Close button");
  await unmount();
  win.sessionStorage.clear();

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
      () => Boolean(host.querySelector("#billing-open-plans")),
      "zh-TW billing heading and overview",
    ),
  );
  assert(host.textContent?.includes("帳單與方案"), "zh-TW billing heading rendered");
  assert(host.textContent?.includes("升級方案"), "zh-TW Upgrade CTA rendered");
  assert(host.textContent?.includes("加購額外用量"), "zh-TW Add extra usage CTA rendered");
  assert(host.textContent?.includes("發票與付款方式"), "zh-TW Invoices & payment details rendered");
  assert(!host.textContent?.includes("計畫") && !host.textContent?.includes("按月"), "zh-TW avoids collided native task strings");

  await React.act(async () => (host.querySelector("#billing-open-plans") as HTMLButtonElement).click());
  const zhPlansModal = host.querySelector('[data-testid="billing-plans-modal"]');
  assert(zhPlansModal?.textContent?.includes("訂閱方案"), "zh-TW Subscription plans label rendered without Plans collision");
  assert(
    zhPlansModal?.textContent?.includes("月繳") && zhPlansModal?.textContent?.includes("年繳"),
    "zh-TW Monthly/Yearly billing labels rendered without collision",
  );
  assert(zhPlansModal?.textContent?.includes("≈ HK$"), "zh-TW secondary HKD reference shown");
  assert(zhPlansModal?.textContent?.includes("以美元 (USD) 經 Stripe 結算，港幣金額僅供參考。"), "zh-TW disclosure shown");
  await React.act(async () => {
    win.document.dispatchEvent(new win.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  });

  const portalBtn = Array.from(host.querySelectorAll('[data-testid="billing-manage-strip"] button')).find(
    (b) => b.textContent === "開啟帳單入口網站",
  ) as HTMLButtonElement;
  assert(portalBtn, "active Stripe subscription shows Open billing portal button");
  nextPostPayload = { url: "https://billing.stripe.com/p/session/bps_synth_1" };
  await React.act(async () => portalBtn.click());
  const secondRedirect = await redirectSignal.wait(React.act, "portal redirect");
  equal(secondRedirect, "https://billing.stripe.com/p/session/bps_synth_1", "portal opens billing.stripe.com");
  await unmount();
  win.localStorage.removeItem("cadence-locale");

  // 3. An uncorrelated old cancellation is honest and cannot start a second payment.
  win.sessionStorage.clear();
  win.history.replaceState({}, "", "https://acme.cadencecloud.app/settings/account?subscription=cancel");
  nextBillingPayload = syntheticSnapshot;
  unmount = await mountView(() =>
    waitForDom(
      React.act,
      host,
      () => Boolean(host.textContent?.includes("You left Checkout")),
      "uncorrelated Checkout return banner",
    ),
  );
  assert(!host.textContent?.includes("No charge was made."), "cancel return does not imply payment status");
  win.sessionStorage.clear();
  await unmount();
  win.history.replaceState({}, "", "/settings/account");

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
  assert(!host.querySelector('[data-testid="billing-usage-summary"]'), "member hides usage summary");
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
      () => Boolean(host.querySelector('[data-testid="billing-manage-strip"]')),
      "IAP notice strip",
    ),
  );
  assert(
    host.querySelector('[data-testid="billing-manage-strip"]')?.textContent?.includes("App Store"),
    "IAP store notice shown on billing manage strip",
  );
  const manageBtns = Array.from(host.querySelectorAll("button")).filter((b) => b.textContent === "Open billing portal");
  equal(manageBtns.length, 0, "IAP subscription hides Stripe portal button");
  await React.act(async () => (host.querySelector("#billing-open-plans") as HTMLButtonElement).click());
  assert(
    host.querySelector('[data-testid="billing-plans-modal"]')?.textContent?.includes("App Store"),
    "IAP store notice shown in plans modal",
  );
  await unmount();

  // 6. Standby extra usage, 14-day chart filter, 3 tabs & pagination
  const daily8Days = Array.from({ length: 8 }, (_, idx) => {
    const pct = Number((1.5 + idx * 0.5).toFixed(1));
    return {
      date: `2026-11-${String(15 + idx).padStart(2, "0")}`,
      totalPercent: pct,
      monthlyPercent: pct,
      extraPercent: 0,
      runCount: idx + 1,
      models: ["Claude Sonnet"],
      source: "monthly" as const,
    };
  });
  nextBillingPayload = {
    ...syntheticSnapshot,
    account: {
      ...syntheticSnapshot.account,
      plan: { name: "Synth Basic", status: "active", provider: "stripe", planId: "synth_basic_mo" },
      trial: undefined,
      usageSummary: {
        mode: "monthly_standby" as const,
        cycle: { usedPercent: 38, remainingPercent: 62, resetsAt: "2026-12-01T00:00:00Z" },
        extra: { hasReserve: true, poolRemainingPercent: 100, poolUsedPercent: 0, status: "standby" as const },
        denominatorBasis: "monthly_plan" as const,
      },
    },
    usage: {
      rows: [
        {
          date: "2026-11-22T11:00:00Z",
          description: "Model usage",
          amount: "-1.500000",
          kind: "charge" as const,
          model: "Claude Sonnet",
          source: "monthly" as const,
          usagePercent: 8.8,
        },
        {
          date: "2026-11-21T08:00:00Z",
          description: "Wallet credit",
          amount: "15.000000",
          kind: "credit" as const,
          model: null,
          source: "extra" as const,
          usagePercent: null,
        },
      ],
      cursor: "cur_page_2",
      dailyUsage: daily8Days,
    },
  };
  unmount = await mountView(() =>
    waitForDom(
      React.act,
      host,
      () => Boolean(host.querySelector('[data-testid="billing-extra-standby"]')),
      "standby strip and daily chart",
    ),
  );
  equal(
    host.querySelector('[data-testid="billing-usage-percent"]')?.textContent?.trim(),
    "38%",
    "active monthly plan shows 38% used",
  );
  assert(
    host.querySelector('[data-testid="billing-extra-card"]')?.textContent?.includes("Standby"),
    "standby badge shown in extra card when monthly < 100%",
  );
  assert(
    host.querySelector('[data-testid="billing-extra-standby"]')?.textContent?.includes("100%") &&
      host.querySelector('[data-testid="billing-extra-standby"]')?.textContent?.includes("left in reserve"),
    "standby extra card shows 100% left in reserve",
  );
  assert(Boolean(host.querySelector('[data-testid="billing-usage-chart"]')), "14-day chart rendered");
  assert(
    host.querySelector('[data-testid="billing-pagination"]')?.textContent?.includes("Page 1 of 2"),
    "daily summary paginates 7 days per page",
  );
  const nextDailyBtn = Array.from(
    host.querySelectorAll('[data-testid="billing-pagination"] button'),
  ).find((b) => b.textContent === "Next") as HTMLButtonElement;
  await React.act(async () => nextDailyBtn.click());
  assert(
    host.querySelector('[data-testid="billing-pagination"]')?.textContent?.includes("Page 2 of 2"),
    "daily summary advances to page 2",
  );

  // Click chart day bar to filter, then switch to Activity log & fetch cursor page
  const dayBar = host.querySelector('[data-chart-day="2026-11-22"]') as HTMLButtonElement;
  await React.act(async () => dayBar.click());
  assert(Boolean(host.querySelector('[data-testid="billing-day-filter"]')), "day filter chip appears after chart click");

  const logTabBtn = host.querySelector('[data-usage-tab="log"]') as HTMLButtonElement;
  await React.act(async () => logTabBtn.click());
  assert(host.textContent?.includes("Claude Sonnet"), "activity log renders model name");
  assert(host.textContent?.includes("8.8%"), "activity log renders usage percentage");

  // Clear filter and fetch older cursor page
  const clearFilterBtn = host.querySelector('[data-testid="billing-day-filter"]') as HTMLButtonElement;
  await React.act(async () => clearFilterBtn.click());
  const nextLogBtn = Array.from(
    host.querySelectorAll('[data-testid="billing-pagination"] button'),
  ).find((b) => b.textContent === "Next") as HTMLButtonElement;
  await React.act(async () => nextLogBtn.click());
  await waitForDom(
    React.act,
    host,
    () => Boolean(host.textContent?.includes("Claude Haiku")),
    "cursor page fetched",
  );

  const eventsTabBtn = host.querySelector('[data-usage-tab="events"]') as HTMLButtonElement;
  await React.act(async () => eventsTabBtn.click());
  assert(host.textContent?.includes("Refill added"), "billing events tab shows refill milestone");
  await unmount();

  // 7. Overflow & Depleted lifecycle states + inactive plan renders "No active plan"
  nextBillingPayload = {
    ...syntheticSnapshot,
    account: {
      ...syntheticSnapshot.account,
      plan: { name: "Synth Basic", status: "active", provider: "stripe", planId: "synth_basic_mo" },
      trial: undefined,
      usageSummary: {
        mode: "overflow" as const,
        cycle: { usedPercent: 100, remainingPercent: 0, resetsAt: "2026-12-01T00:00:00Z" },
        extra: { hasReserve: true, poolRemainingPercent: 79, poolUsedPercent: 21, status: "active" as const },
        denominatorBasis: "monthly_plan" as const,
      },
    },
  };
  unmount = await mountView(() =>
    waitForDom(
      React.act,
      host,
      () => Boolean(host.querySelector('[data-testid="billing-overflow-banner"]')),
      "overflow billing summary",
    ),
  );
  equal(
    host.querySelector('[data-testid="billing-usage-percent"]')?.textContent?.trim(),
    "100%",
    "overflow keeps monthly plan at 100% used",
  );
  equal(
    host.querySelector('[data-testid="billing-extra-percent"]')?.textContent?.trim(),
    "21%",
    "overflow displays active extra reserve used %",
  );
  assert(
    host.querySelector('[data-testid="billing-extra-card"]')?.textContent?.includes("79% remaining"),
    "overflow shows 79% extra remaining",
  );
  await unmount();

  nextBillingPayload = {
    ...syntheticSnapshot,
    account: {
      ...syntheticSnapshot.account,
      plan: { name: "Synth Pro", status: "inactive" },
      trial: { status: "expired", startedAt: "2026-11-01T00:00:00Z", deadlineAt: "2026-11-08T00:00:00Z", credit: "0.000000", tokenCap: 100000, tokensUsed: 100000 },
      balance: { amount: "0.000000", currency: "USD" as const, low: true, zero: true, includedAmount: "0.000000", refillAmount: "0.000000" },
      usageSummary: {
        mode: "depleted" as const,
        cycle: null,
        extra: { hasReserve: false, poolRemainingPercent: 0, poolUsedPercent: 0, status: "depleted" as const },
        denominatorBasis: "standard_month" as const,
      },
    },
  };
  unmount = await mountView(() =>
    waitForDom(
      React.act,
      host,
      () => Boolean(host.querySelector('[data-testid="billing-depleted-banner"]')),
      "depleted billing summary",
    ),
  );
  assert(
    host.querySelector('[data-testid="billing-plan-card"]')?.textContent?.includes("No active plan"),
    "inactive subscription does not display stale plan name",
  );
  assert(
    !host.querySelector('[data-testid="billing-plan-card"]')?.textContent?.includes("Synth Pro"),
    "stale inactive plan name is hidden",
  );
  await unmount();

  // 8. 404 graceful degradation
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

declare const process: { cwd(): string; exit: (code?: number) => void };
run().then(
  () => process.exit(0),
  (err) => {
    console.error(err);
    process.exit(1);
  },
);
