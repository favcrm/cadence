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

/** Preserve the platform's decimal precision; no floating-point money conversion. */
export function displayAmount(amount: string): string {
  return amount.replace(/(\.\d*?[1-9])0+$|\.0+$/, "$1");
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
