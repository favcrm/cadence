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
