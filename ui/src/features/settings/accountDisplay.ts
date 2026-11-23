export interface PlatformAccount {
  configured: boolean;
  manage_url: string | null;
  account: {
    company: { name: string };
    balance: { amount: string; currency: string; low: boolean; zero: boolean };
    plan: { name: string; status: string } | null;
  } | null;
  usage: { date: string; description: string; amount: string; kind: string }[];
  account_error: string | null;
  usage_error: string | null;
}

export interface HostedBillingBalance {
  amount: string;
  currency: "USD";
  low: boolean;
  zero: boolean;
  includedAmount: string;
  refillAmount: string;
}

export interface HostedBillingPlanSummary {
  name: string;
  status: "inactive" | "active" | "past_due";
  provider?: "stripe" | "apple" | "google";
  planId?: string;
}

export interface HostedBillingPack {
  id: string;
  label: string;
  chargeMinor: number;
  credit: string;
  available: boolean;
}

export interface HostedBillingTrial {
  status: "active" | "expired" | "failed";
  startedAt: string;
  deadlineAt: string;
  credit: string;
  tokenCap: number;
  tokensUsed: number;
}

export interface HostedBillingOfferPlan {
  id: string;
  label: string;
  amount: string;
  currency: "USD";
  period: "month" | "year";
  includedCredit: string;
  creditSlices?: number;
  grantsEnabled: boolean;
}

export interface HostedBillingOfferTrialTerms {
  policyId: string;
  durationHours: number;
  credit: string;
  tokenCap: number;
}

export interface HostedBillingOffer {
  asOf: string;
  plan: HostedBillingOfferPlan | null;
  plans?: HostedBillingOfferPlan[];
  trial: {
    state: "active" | "expired" | "failed" | "not_started" | "unavailable";
    eligibility: "used" | "unavailable" | "unproven" | "eligible" | "unknown";
    eligibilityReason: string;
    startedAt: string | null;
    deadlineAt: string | null;
    credit: string | null;
    tokenCap: number | null;
    tokensUsed: number | null;
    terms: HostedBillingOfferTrialTerms | null;
  };
  subscription: {
    state: "none" | "active" | "past_due" | "inactive" | "unknown";
    provider?: "stripe" | "apple" | "google";
    planId?: string;
    planLabel?: string;
    period?: "month" | "year";
  };
  checkout: {
    id: string | null;
    state: "none" | "pending" | "open" | "completed" | "settled" | "expired" | "unknown";
    sessionId: string | null;
    url: string | null;
    expiresAt: string | null;
    available: boolean;
    blocked: string | null;
  };
}

export interface HostedBillingUsageRow {
  date: string;
  description: string;
  amount: string;
  kind: "credit" | "debit" | "charge" | "grant";
}

export interface HostedBillingAccount {
  company: { name: string };
  balance: HostedBillingBalance;
  plan: HostedBillingPlanSummary | null;
  links: { account: string; topUp?: string };
  packs: HostedBillingPack[];
  trial: HostedBillingTrial | null;
}

export interface HostedBillingSnapshot {
  role: "owner" | "member";
  canManage: boolean;
  workspace: {
    id: string;
    name: string;
    nameZh: string | null;
    slug: string | null;
  };
  account: HostedBillingAccount | null;
  offer: HostedBillingOffer | null;
  receivable: string | null;
  usage: {
    rows: HostedBillingUsageRow[];
    cursor: string | null;
  };
  manageUrl: string | null;
}

export interface HostedWorkspaceItem {
  workspaceId: string;
  slug: string;
  name: string;
  nameZh: string | null;
  role: "owner" | "member";
  boardHost: string;
  boardUrl: string;
  boardState: string;
  current: boolean;
}

export interface HostedWorkspacesPayload {
  currentWorkspaceId: string;
  createWorkspaceUrl: string | null;
  workspaces: HostedWorkspaceItem[];
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

const SCALED6_RE = /^(0|[1-9]\d{0,9})\.(\d{6})$/;
const SIGNED_SCALED6_RE = /^([+-]?)(0|[1-9]\d{0,9})\.(\d{6})$/;
const FACTOR = 1_000_000n;
const CREDIT_HUNDREDTH_MICRO = 100n;
const HKD_PEG_NUM = 78n;
const HKD_PEG_DEN = 1000n;

function parseScaled6(amount: string): bigint | null {
  const match = SCALED6_RE.exec(amount);
  if (!match) return null;
  return BigInt(match[1]) * FACTOR + BigInt(match[2]);
}

function isScaled6(value: unknown): value is string {
  return typeof value === "string" && SCALED6_RE.test(value);
}

function isSignedScaled6(value: unknown): value is string {
  return typeof value === "string" && SIGNED_SCALED6_RE.test(value);
}

function isNullableString(value: unknown): value is string | null {
  return value === null || typeof value === "string";
}

function isAccount(value: unknown): value is NonNullable<PlatformAccount["account"]> {
  if (value === null || typeof value !== "object" || Array.isArray(value)
    || !("company" in value) || !("balance" in value) || !("plan" in value)) return false;
  const { company, balance, plan } = value;
  return company !== null && typeof company === "object"
    && "name" in company && typeof company.name === "string"
    && balance !== null && typeof balance === "object"
    && "amount" in balance && typeof balance.amount === "string"
    && "currency" in balance && typeof balance.currency === "string"
    && "low" in balance && typeof balance.low === "boolean"
    && "zero" in balance && typeof balance.zero === "boolean"
    && (plan === null || (typeof plan === "object"
      && "name" in plan && typeof plan.name === "string"
      && "status" in plan && typeof plan.status === "string"));
}

/** Validate the account wire shape without inventing missing financial values. */
export function isPlatformAccount(value: unknown): value is PlatformAccount {
  if (value === null || typeof value !== "object" || Array.isArray(value)
    || !("configured" in value) || !("manage_url" in value) || !("account" in value)
    || !("usage" in value) || !("account_error" in value) || !("usage_error" in value)) return false;
  return typeof value.configured === "boolean"
    && (value.manage_url === null || typeof value.manage_url === "string")
    && (value.account === null || isAccount(value.account))
    && Array.isArray(value.usage)
    && value.usage.every((row: unknown) => row !== null && typeof row === "object"
      && "date" in row && typeof row.date === "string"
      && "description" in row && typeof row.description === "string"
      && "amount" in row && typeof row.amount === "string"
      && "kind" in row && typeof row.kind === "string")
    && (value.account_error === null || typeof value.account_error === "string")
    && (value.usage_error === null || typeof value.usage_error === "string");
}

function parseOfferPlan(value: unknown): HostedBillingOfferPlan | null {
  if (
    !isRecord(value) ||
    typeof value.id !== "string" ||
    typeof value.label !== "string" ||
    !isScaled6(value.amount) ||
    value.currency !== "USD" ||
    (value.period !== "month" && value.period !== "year") ||
    !isScaled6(value.includedCredit) ||
    typeof value.grantsEnabled !== "boolean"
  ) {
    return null;
  }
  if (
    value.creditSlices !== undefined &&
    (typeof value.creditSlices !== "number" || !Number.isInteger(value.creditSlices) || value.creditSlices < 1)
  ) {
    return null;
  }
  return {
    id: value.id,
    label: value.label,
    amount: value.amount,
    currency: "USD",
    period: value.period,
    includedCredit: value.includedCredit,
    ...(value.creditSlices !== undefined ? { creditSlices: value.creditSlices } : {}),
    grantsEnabled: value.grantsEnabled,
  };
}

function parseTrial(value: unknown): HostedBillingTrial | null {
  if (
    !isRecord(value) ||
    (value.status !== "active" && value.status !== "expired" && value.status !== "failed") ||
    typeof value.startedAt !== "string" ||
    typeof value.deadlineAt !== "string" ||
    !isScaled6(value.credit) ||
    typeof value.tokenCap !== "number" ||
    !Number.isInteger(value.tokenCap) ||
    value.tokenCap <= 0 ||
    typeof value.tokensUsed !== "number" ||
    !Number.isInteger(value.tokensUsed) ||
    value.tokensUsed < 0
  ) {
    return null;
  }
  return {
    status: value.status,
    startedAt: value.startedAt,
    deadlineAt: value.deadlineAt,
    credit: value.credit,
    tokenCap: value.tokenCap,
    tokensUsed: value.tokensUsed,
  };
}

function parsePack(value: unknown): HostedBillingPack | null {
  if (
    !isRecord(value) ||
    typeof value.id !== "string" ||
    typeof value.label !== "string" ||
    typeof value.chargeMinor !== "number" ||
    !Number.isInteger(value.chargeMinor) ||
    value.chargeMinor <= 0 ||
    !isScaled6(value.credit) ||
    typeof value.available !== "boolean"
  ) {
    return null;
  }
  return {
    id: value.id,
    label: value.label,
    chargeMinor: value.chargeMinor,
    credit: value.credit,
    available: value.available,
  };
}

function parseHostedAccount(value: unknown): HostedBillingAccount | null {
  if (!isRecord(value) || !isRecord(value.company) || typeof value.company.name !== "string") return null;
  const b = value.balance;
  if (
    !isRecord(b) ||
    !isScaled6(b.amount) ||
    b.currency !== "USD" ||
    typeof b.low !== "boolean" ||
    typeof b.zero !== "boolean" ||
    !isScaled6(b.includedAmount) ||
    !isScaled6(b.refillAmount)
  ) {
    return null;
  }
  let plan: HostedBillingPlanSummary | null = null;
  if (value.plan !== null) {
    if (
      !isRecord(value.plan) ||
      typeof value.plan.name !== "string" ||
      (value.plan.status !== "inactive" && value.plan.status !== "active" && value.plan.status !== "past_due")
    ) {
      return null;
    }
    if (
      value.plan.provider !== undefined &&
      value.plan.provider !== "stripe" &&
      value.plan.provider !== "apple" &&
      value.plan.provider !== "google"
    ) {
      return null;
    }
    if (value.plan.planId !== undefined && typeof value.plan.planId !== "string") return null;
    plan = {
      name: value.plan.name,
      status: value.plan.status,
      ...(value.plan.provider !== undefined ? { provider: value.plan.provider } : {}),
      ...(value.plan.planId !== undefined ? { planId: value.plan.planId } : {}),
    };
  }
  if (!isRecord(value.links) || typeof value.links.account !== "string") return null;
  if (value.links.topUp !== undefined && typeof value.links.topUp !== "string") return null;
  if (!Array.isArray(value.packs)) return null;
  const packs: HostedBillingPack[] = [];
  for (const rawPack of value.packs) {
    const pack = parsePack(rawPack);
    if (!pack) return null;
    packs.push(pack);
  }
  let trial: HostedBillingTrial | null = null;
  if (value.trial !== null) {
    trial = parseTrial(value.trial);
    if (!trial) return null;
  }
  return {
    company: { name: value.company.name },
    balance: {
      amount: b.amount,
      currency: "USD",
      low: b.low,
      zero: b.zero,
      includedAmount: b.includedAmount,
      refillAmount: b.refillAmount,
    },
    plan,
    links: {
      account: value.links.account,
      ...(value.links.topUp !== undefined ? { topUp: value.links.topUp } : {}),
    },
    packs,
    trial,
  };
}

function parseOffer(value: unknown): HostedBillingOffer | null {
  if (!isRecord(value) || typeof value.asOf !== "string") return null;
  let plan: HostedBillingOfferPlan | null = null;
  if (value.plan !== null) {
    plan = parseOfferPlan(value.plan);
    if (!plan) return null;
  }
  let plans: HostedBillingOfferPlan[] | undefined;
  if (value.plans !== undefined) {
    if (!Array.isArray(value.plans)) return null;
    plans = [];
    for (const rawPlan of value.plans) {
      const parsed = parseOfferPlan(rawPlan);
      if (!parsed) return null;
      plans.push(parsed);
    }
  }
  if (!isRecord(value.trial) || !isRecord(value.subscription) || !isRecord(value.checkout)) return null;
  if (typeof value.checkout.available !== "boolean") return null;
  if (!isNullableString(value.checkout.id) || !isNullableString(value.checkout.url)) return null;
  if (!isNullableString(value.checkout.sessionId) || !isNullableString(value.checkout.expiresAt)) return null;
  if (!isNullableString(value.checkout.blocked)) return null;
  return {
    asOf: value.asOf,
    plan,
    ...(plans !== undefined ? { plans } : {}),
    trial: value.trial as HostedBillingOffer["trial"],
    subscription: value.subscription as HostedBillingOffer["subscription"],
    checkout: value.checkout as HostedBillingOffer["checkout"],
  };
}

/** Strict runtime validator for `GET /api/account/billing` unwrapped payload. */
export function parseHostedBilling(value: unknown): HostedBillingSnapshot | null {
  if (!isRecord(value)) return null;
  if ((value.role !== "owner" && value.role !== "member") || typeof value.canManage !== "boolean") {
    return null;
  }
  const ws = value.workspace;
  if (
    !isRecord(ws) ||
    typeof ws.id !== "string" ||
    typeof ws.name !== "string" ||
    !isNullableString(ws.nameZh) ||
    !isNullableString(ws.slug)
  ) {
    return null;
  }
  const account = value.account === null ? null : parseHostedAccount(value.account);
  if (value.account !== null && !account) return null;
  const offer = value.offer === null ? null : parseOffer(value.offer);
  if (value.offer !== null && !offer) return null;
  if (value.receivable !== null && !isScaled6(value.receivable)) return null;
  if (!isRecord(value.usage) || !Array.isArray(value.usage.rows) || !isNullableString(value.usage.cursor)) {
    return null;
  }
  const rows: HostedBillingUsageRow[] = [];
  for (const row of value.usage.rows) {
    if (
      !isRecord(row) ||
      typeof row.date !== "string" ||
      typeof row.description !== "string" ||
      !isSignedScaled6(row.amount) ||
      (row.kind !== "credit" && row.kind !== "debit" && row.kind !== "charge" && row.kind !== "grant")
    ) {
      return null;
    }
    rows.push({
      date: row.date,
      description: row.description,
      amount: row.amount,
      kind: row.kind,
    });
  }
  if (!isNullableString(value.manageUrl)) return null;
  return {
    role: value.role,
    canManage: value.canManage,
    workspace: { id: ws.id, name: ws.name, nameZh: ws.nameZh, slug: ws.slug },
    account,
    offer,
    receivable: value.receivable,
    usage: { rows, cursor: value.usage.cursor },
    manageUrl: value.manageUrl,
  };
}

/** Strict runtime validator for `GET /api/account/workspaces` unwrapped payload. */
export function parseHostedWorkspaces(value: unknown): HostedWorkspacesPayload | null {
  if (
    !isRecord(value) ||
    typeof value.currentWorkspaceId !== "string" ||
    !isNullableString(value.createWorkspaceUrl) ||
    !Array.isArray(value.workspaces)
  ) {
    return null;
  }
  const workspaces: HostedWorkspaceItem[] = [];
  for (const row of value.workspaces) {
    if (
      !isRecord(row) ||
      typeof row.workspaceId !== "string" ||
      typeof row.slug !== "string" ||
      typeof row.name !== "string" ||
      !isNullableString(row.nameZh) ||
      (row.role !== "owner" && row.role !== "member") ||
      typeof row.boardHost !== "string" ||
      typeof row.boardUrl !== "string" ||
      typeof row.boardState !== "string" ||
      typeof row.current !== "boolean"
    ) {
      return null;
    }
    workspaces.push({
      workspaceId: row.workspaceId,
      slug: row.slug,
      name: row.name,
      nameZh: row.nameZh,
      role: row.role,
      boardHost: row.boardHost,
      boardUrl: row.boardUrl,
      boardState: row.boardState,
      current: row.current,
    });
  }
  return {
    currentWorkspaceId: value.currentWorkspaceId,
    createWorkspaceUrl: value.createWorkspaceUrl,
    workspaces,
  };
}

/** Extract and validate the Stripe redirect URL returned by subscribe, topup, or portal. */
export function parseCheckoutUrlResponse(value: unknown): string | null {
  if (!isRecord(value) || typeof value.url !== "string") return null;
  return safeStripeCheckoutUrl(value.url);
}

/** Preserve the platform's decimal precision; no floating-point money conversion. */
export function displayAmount(amount: string): string {
  return amount.replace(/(\.\d*?[1-9])0+$|\.0+$/, "$1");
}

function formatCreditMagnitude(absMicro: bigint, locale: string): string {
  if (absMicro === 0n) {
    return new Intl.NumberFormat(locale, { maximumFractionDigits: 0 }).format(0);
  }
  if (absMicro < CREDIT_HUNDREDTH_MICRO) {
    const min = new Intl.NumberFormat(locale, {
      minimumFractionDigits: 2,
      maximumFractionDigits: 2,
    }).format(0.01);
    return `< ${min}`;
  }
  const hundredths = (absMicro + CREDIT_HUNDREDTH_MICRO / 2n) / CREDIT_HUNDREDTH_MICRO;
  const fractionDigits = hundredths % 100n === 0n ? 0 : hundredths % 10n === 0n ? 1 : 2;
  return new Intl.NumberFormat(locale, {
    minimumFractionDigits: fractionDigits,
    maximumFractionDigits: 2,
  }).format(Number(hundredths) / 100);
}

/** Convert a scale-6 USD string into customer credits (`100 credits = US$1.00`). */
export function formatCredits(scaled6: string, locale = "en"): string | null {
  const micro = parseScaled6(scaled6);
  if (micro === null) return null;
  return formatCreditMagnitude(micro, locale);
}

/** Convert a signed scale-6 USD ledger delta into signed credits. */
export function formatSignedCredits(signedScaled6: string, locale = "en"): string | null {
  const negative = signedScaled6.startsWith("-");
  const unsigned = negative || signedScaled6.startsWith("+") ? signedScaled6.slice(1) : signedScaled6;
  const micro = parseScaled6(unsigned);
  if (micro === null) return null;
  if (micro === 0n) return formatCreditMagnitude(0n, locale);
  return `${negative ? "-" : "+"}${formatCreditMagnitude(micro, locale)}`;
}

function formatUsdCents(cents: bigint, locale: string): string {
  const fractionDigits = cents % 100n === 0n ? 0 : 2;
  return new Intl.NumberFormat(locale, {
    style: "currency",
    currency: "USD",
    currencyDisplay: "narrowSymbol",
    minimumFractionDigits: fractionDigits,
    maximumFractionDigits: 2,
  }).format(Number(cents) / 100);
}

/** Format a scale-6 USD string as USD (`$12` or `$12.50`). */
export function formatUsdFromScaled(scaled6: string, locale = "en-US"): string | null {
  const micro = parseScaled6(scaled6);
  if (micro === null) return null;
  const cents = (micro + 5_000n) / 10_000n;
  return formatUsdCents(cents, locale);
}

/** Format positive integer USD cents as USD (`$15` or `$15.50`). */
export function formatUsdFromMinor(chargeMinor: number, locale = "en-US"): string | null {
  if (!Number.isSafeInteger(chargeMinor) || chargeMinor < 0) return null;
  return formatUsdCents(BigInt(chargeMinor), locale);
}

/** Secondary reference-only `≈ HK$...` display from USD scale-6 (7.8 peg). */
export function formatApproxHkdFromScaled(scaled6: string): string | null {
  const micro = parseScaled6(scaled6);
  if (micro === null) return null;
  const cents = (micro + 5_000n) / 10_000n;
  const hkd = (cents * HKD_PEG_NUM + HKD_PEG_DEN / 2n) / HKD_PEG_DEN;
  return `≈ HK$${hkd.toString()}`;
}

/** Secondary reference-only `≈ HK$...` display from positive integer USD cents (7.8 peg). */
export function formatApproxHkdFromMinor(chargeMinor: number): string | null {
  if (!Number.isSafeInteger(chargeMinor) || chargeMinor < 0) return null;
  const hkd = (BigInt(chargeMinor) * HKD_PEG_NUM + HKD_PEG_DEN / 2n) / HKD_PEG_DEN;
  return `≈ HK$${hkd.toString()}`;
}

/** Integer-derived percentage split between included/trial credits and non-expiring refills. */
export function creditSplitPercentages(
  includedScaled6: string,
  refillScaled6: string,
): { includedPct: number; refillPct: number } {
  const included = parseScaled6(includedScaled6) ?? 0n;
  const refill = parseScaled6(refillScaled6) ?? 0n;
  const total = included + refill;
  if (total <= 0n) return { includedPct: 0, refillPct: 0 };
  const includedPct = Number((included * 100n) / total);
  const refillPct = Number((refill * 100n) / total);
  return { includedPct, refillPct };
}

/** Defense in depth: only redirect billing actions to Stripe-hosted HTTPS URLs. */
export function safeStripeCheckoutUrl(value: string | null | undefined): string | null {
  if (!value) return null;
  try {
    const url = new URL(value);
    if (url.protocol !== "https:" || url.username || url.password) return null;
    const host = url.hostname.toLowerCase();
    return host === "checkout.stripe.com" || host === "billing.stripe.com" || host.endsWith(".stripe.com")
      ? url.href
      : null;
  } catch {
    return null;
  }
}

/** Defense in depth: only navigate to hosted Cadence boards or the central platform web origin. */
export function safeWorkspaceUrl(value: string | null | undefined): string | null {
  if (!value) return null;
  try {
    const url = new URL(value);
    if (url.protocol !== "https:" || url.username || url.password) return null;
    const host = url.hostname.toLowerCase();
    return host === "app-v2.agenticos.hk" || host === "app.agenticos.hk" || host.endsWith(".cadencecloud.app")
      ? url.href
      : null;
  } catch {
    return null;
  }
}

/** Parse Stripe return parameters (`?subscription=success|cancel` or `?topup=success|cancel`). */
export function readBillingReturnParams(
  search: string,
): { kind: "subscription" | "topup"; status: "success" | "cancel" } | null {
  const params = new URLSearchParams(search);
  const sub = params.get("subscription");
  if (sub === "success" || sub === "cancel") {
    return { kind: "subscription", status: sub };
  }
  const topup = params.get("topup");
  if (topup === "success" || topup === "cancel") {
    return { kind: "topup", status: topup };
  }
  return null;
}

/** Defense in depth: the only account navigation supported by this board. */
export function safeManageUrl(value: string | null): string | null {
  if (!value) return null;
  try {
    const url = new URL(value);
    const keys: string[] = [];
    url.searchParams.forEach((_, key) => keys.push(key));
    const company = url.searchParams.get("company");
    return url.origin === "https://app-v2.agenticos.hk" && url.pathname === "/account"
      && !url.username && !url.password && !url.hash
      && keys.length === 1 && keys[0] === "company"
      && company !== null && /^[a-z0-9][a-z0-9-]{0,62}$/.test(company)
      ? url.href : null;
  } catch {
    return null;
  }
}
