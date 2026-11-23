import { useCallback, useEffect, useState } from "react";
import Button from "../../ui/Button";
import { IconRefresh } from "../../ui/icons";
import { sessionHeaders } from "../../lib/sessionKey";
import { privateFetch, withPrivateOperation } from "../../lib/privateSession";
import {
  creditSplitPercentages,
  displayAmount,
  formatApproxHkdFromMinor,
  formatApproxHkdFromScaled,
  formatCredits,
  formatSignedCredits,
  formatUsdFromMinor,
  formatUsdFromScaled,
  isPlatformAccount,
  parseCheckoutUrlResponse,
  parseHostedBilling,
  readBillingReturnParams,
  safeManageUrl,
  safeStripeCheckoutUrl,
  type HostedBillingOfferPlan,
  type HostedBillingSnapshot,
  type PlatformAccount as Account,
} from "./accountDisplay";
import "./account.css";
import { useLocale, useTranslate } from "../../lib/locale";

export default function PlatformAccount({
  hosted = false,
  onBillingSnapshot,
  onRedirect,
}: {
  hosted?: boolean;
  onBillingSnapshot?: (snapshot: HostedBillingSnapshot | null) => void;
  onRedirect?: (url: string) => void;
} = {}) {
  return hosted ? (
    <HostedBilling onBillingSnapshot={onBillingSnapshot} onRedirect={onRedirect} />
  ) : (
    <NativePlatformAccount />
  );
}

function NativePlatformAccount() {
  const t = useTranslate();
  const [data, setData] = useState<Account | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(false);
  const [checkedAt, setCheckedAt] = useState<Date | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(false);
    try {
      await withPrivateOperation(async (op) => {
        const response = await privateFetch("/api/platform-account", {
          headers: sessionHeaders(),
        }, op);
        if (!response.ok) throw new Error("Account request failed");
        const next: unknown = await response.json();
        op.assertCurrent();
        if (!isPlatformAccount(next)) throw new Error("Account request failed");
        setData(next);
        setCheckedAt(new Date());
      });
    } catch {
      setError(true);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const manage = safeManageUrl(data?.manage_url ?? null);
  const account = data?.account;
  // The platform's usage contract is HKD even when its account read fails.
  const currency = account?.balance.currency || "HKD";

  return (
    <section className="account-workspace" aria-labelledby="account-title" aria-busy={loading}>
      <header className="account-heading">
        <div>
          <h1 id="account-title">{t("Account")}</h1>
          <p>{t("Plan, balance and usage from your connected platform. This page is read-only.")}</p>
          {checkedAt && (
            <p className="account-checked">
              {t("Last checked")} <time dateTime={checkedAt.toISOString()}>{checkedAt.toLocaleString()}</time>
            </p>
          )}
        </div>
        <div className="account-actions">
          <Button icon={<IconRefresh />} loading={loading} onClick={() => void refresh()}>{t("Refresh")}</Button>
          {manage && data?.configured && (
            <a className="btn btn-primary" href={manage} target="_blank" rel="noopener noreferrer">
              {t("Manage account")}<span className="sr-only"> {t("(opens in a new tab)")}</span>
            </a>
          )}
        </div>
      </header>

      {error && (
        <div className="account-notice" data-tone="fail" role="alert">
          <strong>{data ? t("Couldn’t refresh account") : t("Couldn’t load account")}</strong>
          <p>
            {data ? t("Showing the information from the last successful check. Refresh to try again.") : t("Account information is unavailable. Refresh to try again.")}
          </p>
        </div>
      )}

      {!data && loading && (
        <div className="account-state" role="status">{t("Loading account…")}</div>
      )}

      {data?.configured === false && (
        <div className="account-state">
          <h2>{t("No connected platform account")}</h2>
          <p>{t("Connect an account outside this read-only board, then refresh to see its plan and usage.")}</p>
        </div>
      )}

      {data?.configured && (
        <>
          {account?.company.name && (
            <div className="account-identity">
              <span>{t("Connected account")}</span>
              <strong>{account.company.name}</strong>
            </div>
          )}
          {account ? (
            <section className="account-summary" aria-label={t("Account summary")}>
              <article className="card account-summary-card">
                <h2>{t("Available balance")}</h2>
                <p className="account-balance">
                  <span>{displayAmount(account.balance.amount)}</span>
                  <span className="account-currency">{currency}</span>
                </p>
                {account.balance.low && (
                  <p className="account-balance-warning" role="status">
                    {account.balance.zero ? t("No model credit available.") : t("Model credit is running low.")}
                  </p>
                )}
              </article>
              <article className="card account-summary-card">
                <h2>{t("Plan")}</h2>
                <p className="account-plan">{account.plan?.name || t("No plan")}</p>
                <p className="account-plan-status">
                  {account.plan?.status.replaceAll("_", " ") || t("No subscription is configured.")}
                </p>
              </article>
            </section>
          ) : (
            <div className="account-notice" role="status">
              <strong>{t("Balance and plan unavailable")}</strong>
              <p>{data.account_error || t("The connected platform did not return an account summary.")}</p>
            </div>
          )}

          {account && data.account_error && (
            <div className="account-notice" role="alert">
              <strong>{t("Account details may be incomplete")}</strong>
              <p>{data.account_error}</p>
            </div>
          )}

          <section className="account-usage" aria-labelledby="account-usage-title">
            <div className="account-section-heading">
              <h2 id="account-usage-title">{t("Recent usage")}</h2>
              {!data.usage_error && data.usage.length > 0 && (
                <span>{data.usage.length} {data.usage.length === 1 ? t("entry") : t("entries")}</span>
              )}
            </div>
            {data.usage_error ? (
              <div className="account-state" role="alert">
                <strong>{t("Usage unavailable")}</strong>
                <p>{data.usage_error}</p>
              </div>
            ) : data.usage.length === 0 ? (
              <div className="account-state">{t("No recent usage to show.")}</div>
            ) : (
              <div className="card account-usage-table">
                <table>
                  <caption className="sr-only">{t("Recent account usage")}</caption>
                  <thead>
                    <tr><th scope="col">{t("Date")}</th><th scope="col">{t("Description")}</th><th scope="col">{t("Amount")}</th></tr>
                  </thead>
                  <tbody>
                    {data.usage.map((row, index) => (
                      <tr key={`${row.date}-${index}`}>
                        <td><time dateTime={row.date}>{row.date.slice(0, 10)}</time></td>
                        <td>{row.description}</td>
                        <td>{displayAmount(row.amount)}{currency ? ` ${currency}` : ""}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </section>
          <p className="account-members-note">
            {t("Member information is not available from this read-only connection.")}
            {manage && " "}{manage && t("Use Manage account to view and manage members.")}
          </p>
          {manage && (
            <p className="account-members-note">
              {t("Password sign-in, two-factor authentication, and workspace 2FA policy are managed in Manage account.")}{" "}
              <a href={manage} target="_blank" rel="noopener noreferrer">
                {t("Account security settings")}<span className="sr-only"> {t("(opens in a new tab)")}</span>
              </a>
            </p>
          )}
        </>
      )}
    </section>
  );
}

function HostedBilling({
  onBillingSnapshot,
  onRedirect,
}: {
  onBillingSnapshot?: (snapshot: HostedBillingSnapshot | null) => void;
  onRedirect?: (url: string) => void;
}) {
  const { locale, t } = useLocale();
  const [data, setData] = useState<HostedBillingSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [unsupported, setUnsupported] = useState(false);
  const [error, setError] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [checkedAt, setCheckedAt] = useState<Date | null>(null);
  const [interval, setInterval] = useState<"month" | "year">("month");
  const [returnBanner, setReturnBanner] = useState<{ kind: "subscription" | "topup"; status: "success" | "cancel" } | null>(null);

  const navigateExternal = useCallback((url: string) => {
    if (onRedirect) onRedirect(url);
    else window.location.assign(url);
  }, [onRedirect]);

  const refresh = useCallback(async (opts: { background?: boolean; signal?: AbortSignal } = {}) => {
    if (!opts.background) setLoading(true);
    setError(false);
    try {
      await withPrivateOperation(async (op) => {
        const response = await privateFetch("/api/account/billing", {
          headers: sessionHeaders(),
        }, op);
        op.assertCurrent();
        if (response.status === 404 || response.status === 501) {
          setUnsupported(true);
          onBillingSnapshot?.(null);
          return;
        }
        if (!response.ok) throw new Error("Billing request failed");
        const next: unknown = await response.json();
        op.assertCurrent();
        const parsed = parseHostedBilling(next);
        if (!parsed) throw new Error("Billing response malformed");
        setUnsupported(false);
        setData(parsed);
        setCheckedAt(new Date());
        onBillingSnapshot?.(parsed);
      }, opts.signal);
    } catch (err) {
      if (err instanceof DOMException && err.name === "AbortError") return;
      setError(true);
    } finally {
      if (!opts.background) setLoading(false);
    }
  }, [onBillingSnapshot]);

  useEffect(() => {
    const abort = new AbortController();
    const returned = readBillingReturnParams(window.location.search);
    if (returned) {
      setReturnBanner(returned);
      const url = new URL(window.location.href);
      url.searchParams.delete("subscription");
      url.searchParams.delete("topup");
      const cleaned = `${url.pathname}${url.search}${url.hash}`;
      window.history.replaceState(null, "", cleaned);
    }
    void refresh({ signal: abort.signal });

    let timer: ReturnType<typeof setTimeout> | undefined;
    let remaining = returned?.status === "success" ? 5 : 0;
    const poll = () => {
      if (abort.signal.aborted || remaining <= 0) return;
      remaining -= 1;
      timer = setTimeout(() => {
        if (abort.signal.aborted) return;
        void refresh({ background: true, signal: abort.signal }).finally(() => {
          poll();
        });
      }, 2000);
    };
    if (remaining > 0) poll();

    return () => {
      abort.abort();
      if (timer !== undefined) clearTimeout(timer);
    };
  }, [refresh]);

  useEffect(() => {
    const period = data?.offer?.subscription.period;
    if (period === "month" || period === "year") {
      setInterval(period);
    }
  }, [data?.offer?.subscription.period]);

  const runAction = useCallback(async (actionId: string, path: string, body: unknown) => {
    setBusyAction(actionId);
    setActionError(null);
    try {
      await withPrivateOperation(async (op) => {
        const response = await privateFetch(
          path,
          {
            method: "POST",
            headers: {
              ...sessionHeaders(),
              "Content-Type": "application/json",
              "X-Cadence-Board": "1",
            },
            body: JSON.stringify(body),
          },
          op,
        );
        const payload: unknown = await response.json().catch(() => null);
        op.assertCurrent();
        if (!response.ok) {
          const msg =
            payload &&
            typeof payload === "object" &&
            "error" in payload &&
            payload.error &&
            typeof payload.error === "object" &&
            "message" in payload.error &&
            typeof payload.error.message === "string"
              ? payload.error.message
              : t("Unable to start Stripe checkout. Please try again.");
          setActionError(msg);
          return;
        }
        const safeUrl = parseCheckoutUrlResponse(payload);
        if (!safeUrl) {
          setActionError(t("Unable to open billing checkout because the redirect link was invalid."));
          return;
        }
        navigateExternal(safeUrl);
      });
    } catch {
      setActionError(t("Unable to start Stripe checkout. Please try again."));
    } finally {
      setBusyAction(null);
    }
  }, [navigateExternal, t]);

  const account = data?.account ?? null;
  const offer = data?.offer ?? null;
  const workspaceLabel =
    locale === "zh-TW" && data?.workspace.nameZh?.trim()
      ? data.workspace.nameZh.trim()
      : data?.workspace.name ?? "";
  const isZh = locale === "zh-TW";
  const numLocale = isZh ? "zh-Hant-HK" : "en-US";
  const allPlans: HostedBillingOfferPlan[] =
    offer?.plans && offer.plans.length > 0
      ? offer.plans
      : offer?.plan
        ? [offer.plan]
        : [];
  const hasMonthly = allPlans.some((p) => p.period === "month");
  const hasYearly = allPlans.some((p) => p.period === "year");
  const showIntervalToggle = hasMonthly && hasYearly;
  const visiblePlans = showIntervalToggle ? allPlans.filter((p) => p.period === interval) : allPlans;
  const planProvider = account?.plan?.provider ?? offer?.subscription.provider;
  const isIapManaged = planProvider === "apple" || planProvider === "google";
  const subActive = account?.plan?.status === "active" || account?.plan?.status === "past_due";
  const openCheckoutUrl =
    offer?.checkout.state === "open" ? safeStripeCheckoutUrl(offer.checkout.url) : null;
  const split = account
    ? creditSplitPercentages(account.balance.includedAmount, account.balance.refillAmount)
    : { includedPct: 0, refillPct: 0 };

  return (
    <section className="account-workspace" aria-labelledby="account-title" aria-busy={loading}>
      <header className="account-heading">
        <div>
          <h1 id="account-title">{t("Billing")}</h1>
          <p>{t("Manage credits, plans, refill packs and recent usage for this workspace.")}</p>
          {checkedAt && (
            <p className="account-checked">
              {t("Last checked")} <time dateTime={checkedAt.toISOString()}>{checkedAt.toLocaleString()}</time>
            </p>
          )}
        </div>
        <div className="account-actions">
          <Button icon={<IconRefresh />} loading={loading} onClick={() => void refresh()}>
            {t("Refresh")}
          </Button>
          {data?.canManage && account && subActive && !isIapManaged && (
            <Button
              variant="primary"
              loading={busyAction === "portal"}
              disabled={busyAction !== null}
              onClick={() => void runAction("portal", "/api/account/billing/portal", {})}
            >
              {t("Manage billing")}
            </Button>
          )}
        </div>
      </header>

      {returnBanner && (
        <div
          className={`account-notice ${returnBanner.status === "success" ? "billing-notice-ok" : ""}`}
          role="status"
        >
          <strong>
            {returnBanner.status === "success"
              ? returnBanner.kind === "subscription"
                ? t("Subscription updated")
                : t("Top-up payment received")
              : t("Checkout cancelled")}
          </strong>
          <p>
            {returnBanner.status === "success"
              ? t("Your workspace balance and plan status are refreshing.")
              : t("No charge was made. You can pick a plan or refill pack anytime.")}
          </p>
        </div>
      )}

      {actionError && (
        <div className="account-notice" data-tone="fail" role="alert">
          <strong>{t("Billing action failed")}</strong>
          <p>{actionError}</p>
        </div>
      )}

      {error && (
        <div className="account-notice" data-tone="fail" role="alert">
          <strong>{data ? t("Couldn’t refresh billing") : t("Couldn’t load billing")}</strong>
          <p>
            {data
              ? t("Showing the information from the last successful check. Refresh to try again.")
              : t("Billing information is unavailable. Refresh to try again.")}
          </p>
        </div>
      )}

      {unsupported && (
        <div className="account-state" role="status">
          <h2>{t("Billing is not enabled on this board yet.")}</h2>
          <p>{t("Workspace billing controls become available once the billing service is connected.")}</p>
        </div>
      )}

      {!data && !unsupported && loading && (
        <div className="account-state" role="status">{t("Loading billing…")}</div>
      )}

      {data && !unsupported && (
        <>
          {workspaceLabel && (
            <div className="account-identity">
              <span>{t("Workspace")}</span>
              <strong>{workspaceLabel}</strong>
            </div>
          )}

          {(!data.canManage || !account) ? (
            <div className="account-state" data-testid="billing-member-readonly">
              <h2>{t("Billing is managed by workspace owners.")}</h2>
              <p>{t("Only workspace owners can view balances, change plans, or purchase credit packs.")}</p>
            </div>
          ) : (
            <>
              {data.receivable && Number(data.receivable) > 0 && (
                <div className="account-notice" role="status">
                  <strong>{t("Outstanding balance")}</strong>
                  <p>
                    {t("Unsettled provider charges:")}{" "}
                    {formatUsdFromScaled(data.receivable, numLocale) ?? data.receivable}
                  </p>
                </div>
              )}

              {isIapManaged && (
                <div className="account-notice" role="status" data-testid="billing-iap-notice">
                  <strong>
                    {planProvider === "apple"
                      ? t("Managed via App Store")
                      : t("Managed via Google Play")}
                  </strong>
                  <p>
                    {planProvider === "apple"
                      ? t("This workspace subscription is billed through the Apple App Store. Manage or cancel it in your Apple ID subscriptions.")
                      : t("This workspace subscription is billed through Google Play. Manage or cancel it in Google Play subscriptions.")}
                  </p>
                </div>
              )}

              <section className="account-summary" aria-label={t("Account summary")}>
                <article className="card account-summary-card">
                  <div className="billing-card-head">
                    <h2>{t("Available credits")}</h2>
                    {account.packs.length > 0 && (
                      <Button
                        size="sm"
                        onClick={() => document.getElementById("billing-packs")?.scrollIntoView({ behavior: "smooth" })}
                      >
                        {t("Buy refill credits")}
                      </Button>
                    )}
                  </div>
                  <p className="account-balance" data-testid="billing-total-credits">
                    <span>{formatCredits(account.balance.amount, numLocale) ?? account.balance.amount}</span>
                    <span className="billing-unit">{t("credits")}</span>
                  </p>
                  <div className="billing-split-bar" aria-hidden="true">
                    <div className="billing-split-included" style={{ width: `${split.includedPct}%` }} />
                    <div className="billing-split-refill" style={{ width: `${split.refillPct}%` }} />
                  </div>
                  <div className="billing-split-legend">
                    <span className="billing-split-item">
                      <span className="billing-swatch" data-kind="included" aria-hidden="true" />
                      {t("Included / Trial")}:{" "}
                      <strong>{formatCredits(account.balance.includedAmount, numLocale) ?? account.balance.includedAmount}</strong>
                    </span>
                    <span className="billing-split-item">
                      <span className="billing-swatch" data-kind="refill" aria-hidden="true" />
                      {t("Refill credits (never expire)")}:{" "}
                      <strong>{formatCredits(account.balance.refillAmount, numLocale) ?? account.balance.refillAmount}</strong>
                    </span>
                  </div>
                  {account.balance.low && (
                    <p className="account-balance-warning" role="status">
                      {account.balance.zero ? t("No model credit available.") : t("Model credit is running low.")}
                    </p>
                  )}
                </article>

                <article className="card account-summary-card">
                  <div className="billing-card-head">
                    <h2>{t("Plan")}</h2>
                    {account.trial?.status === "active" && (
                      <span className="chip bg-accent/15 text-accent">{t("Free trial")}</span>
                    )}
                  </div>
                  <p className="account-plan">
                    {account.plan?.name || (account.trial?.status === "active" ? t("Free trial") : t("No plan"))}
                  </p>
                  <p className="account-plan-status">
                    {account.plan?.status === "active"
                      ? t("Active")
                      : account.plan?.status === "past_due"
                        ? t("Past due")
                        : account.trial?.status === "active"
                          ? t("Trial active")
                          : t("No active subscription")}
                  </p>
                  {account.trial?.status === "active" && (
                    <p className="billing-trial-meta">
                      {t("Trial ends")}{" "}
                      <time dateTime={account.trial.deadlineAt}>
                        {new Date(account.trial.deadlineAt).toLocaleDateString()}
                      </time>
                      {" · "}
                      {account.trial.tokensUsed.toLocaleString()} / {account.trial.tokenCap.toLocaleString()} {t("tokens")}
                    </p>
                  )}
                  {subActive && !isIapManaged && (
                    <div className="billing-card-actions">
                      <Button
                        size="sm"
                        loading={busyAction === "portal"}
                        disabled={busyAction !== null}
                        onClick={() => void runAction("portal", "/api/account/billing/portal", {})}
                      >
                        {t("Manage subscription")}
                      </Button>
                    </div>
                  )}
                  {openCheckoutUrl && (
                    <div className="billing-card-actions">
                      <a className="btn btn-primary btn-sm" href={openCheckoutUrl}>
                        {t("Resume checkout")}
                      </a>
                    </div>
                  )}
                </article>
              </section>

              {allPlans.length > 0 && (
                <section className="billing-section" aria-labelledby="billing-plans-title">
                  <div className="account-section-heading">
                    <h2 id="billing-plans-title">{t("Plans")}</h2>
                    {showIntervalToggle && (
                      <div className="billing-interval-toggle" role="group" aria-label={t("Billing interval")}>
                        <button
                          type="button"
                          className="billing-interval-btn"
                          aria-pressed={interval === "month"}
                          onClick={() => setInterval("month")}
                        >
                          {t("Monthly")}
                        </button>
                        <button
                          type="button"
                          className="billing-interval-btn"
                          aria-pressed={interval === "year"}
                          onClick={() => setInterval("year")}
                        >
                          {t("Yearly")}
                        </button>
                      </div>
                    )}
                  </div>
                  <div className="billing-catalog-grid">
                    {visiblePlans.map((plan) => {
                      const currentPlan =
                        subActive &&
                        (account.plan?.planId ? account.plan.planId === plan.id : account.plan?.name === plan.label);
                      const usd = formatUsdFromScaled(plan.amount, numLocale) ?? plan.amount;
                      const hkd = isZh ? formatApproxHkdFromScaled(plan.amount) : null;
                      const included = formatCredits(plan.includedCredit, numLocale) ?? plan.includedCredit;
                      const actionKey = `sub:${plan.id}`;
                      return (
                        <article
                          key={plan.id}
                          className="card billing-catalog-card"
                          data-current={currentPlan || undefined}
                          data-plan-id={plan.id}
                        >
                          <div>
                            <div className="billing-card-head">
                              <strong>{plan.label}</strong>
                              {currentPlan && <span className="chip bg-accent/15 text-accent">{t("Current plan")}</span>}
                            </div>
                            <div className="billing-price-row">
                              <span className="billing-price-usd">{usd}</span>
                              <span className="billing-price-period">
                                {plan.period === "year" ? t("/ yr") : t("/ mo")}
                              </span>
                              {hkd && <span className="billing-price-hkd">{hkd}</span>}
                            </div>
                            <p className="billing-included-credits">
                              {included} {t("credits / mo")}
                            </p>
                          </div>
                          <div>
                            {currentPlan ? (
                              <Button full size="sm" disabled>
                                {t("Current plan")}
                              </Button>
                            ) : subActive || isIapManaged ? (
                              <Button
                                full
                                size="sm"
                                disabled={isIapManaged || busyAction !== null}
                                loading={busyAction === "portal"}
                                onClick={() => void runAction("portal", "/api/account/billing/portal", {})}
                              >
                                {t("Manage subscription")}
                              </Button>
                            ) : (
                              <Button
                                full
                                size="sm"
                                variant="primary"
                                disabled={offer?.checkout.available === false || busyAction !== null}
                                loading={busyAction === actionKey}
                                onClick={() => void runAction(actionKey, "/api/account/billing/subscribe", { planId: plan.id })}
                              >
                                {t("Choose plan")}
                              </Button>
                            )}
                          </div>
                        </article>
                      );
                    })}
                  </div>
                </section>
              )}

              {account.packs.length > 0 && (
                <section id="billing-packs" className="billing-section" aria-labelledby="billing-packs-title">
                  <div className="account-section-heading">
                    <h2 id="billing-packs-title">{t("Refill packs")}</h2>
                    <span>{t("Credits never expire")}</span>
                  </div>
                  <div className="billing-catalog-grid">
                    {account.packs.map((pack) => {
                      const usd = formatUsdFromMinor(pack.chargeMinor, numLocale) ?? `${pack.chargeMinor}`;
                      const hkd = isZh ? formatApproxHkdFromMinor(pack.chargeMinor) : null;
                      const creditCount = formatCredits(pack.credit, numLocale) ?? pack.credit;
                      const actionKey = `pack:${pack.id}`;
                      return (
                        <article key={pack.id} className="card billing-catalog-card" data-pack-id={pack.id}>
                          <div>
                            <strong>{pack.label}</strong>
                            <div className="billing-price-row">
                              <span className="billing-price-usd">{usd}</span>
                              {hkd && <span className="billing-price-hkd">{hkd}</span>}
                            </div>
                            <p className="billing-included-credits">
                              {creditCount} {t("credits")}
                            </p>
                          </div>
                          <Button
                            full
                            size="sm"
                            disabled={!pack.available || busyAction !== null}
                            loading={busyAction === actionKey}
                            onClick={() => void runAction(actionKey, "/api/account/billing/topup", { pack: pack.id })}
                          >
                            {pack.available ? t("Buy pack") : t("Unavailable")}
                          </Button>
                        </article>
                      );
                    })}
                  </div>
                </section>
              )}

              {isZh && (allPlans.length > 0 || account.packs.length > 0) && (
                <p className="billing-disclosure">
                  {t("Billed in USD via Stripe. HKD figures are approximate references only.")}
                </p>
              )}

              <section className="account-usage" aria-labelledby="account-usage-title">
                <div className="account-section-heading">
                  <h2 id="account-usage-title">{t("Recent usage")}</h2>
                  {data.usage.rows.length > 0 && (
                    <span>
                      {data.usage.rows.length} {data.usage.rows.length === 1 ? t("entry") : t("entries")}
                    </span>
                  )}
                </div>
                {data.usage.rows.length === 0 ? (
                  <div className="account-state">{t("No recent usage to show.")}</div>
                ) : (
                  <div className="card account-usage-table">
                    <table>
                      <caption className="sr-only">{t("Recent account usage")}</caption>
                      <thead>
                        <tr>
                          <th scope="col">{t("Date")}</th>
                          <th scope="col">{t("Description")}</th>
                          <th scope="col">{t("Credits")}</th>
                        </tr>
                      </thead>
                      <tbody>
                        {data.usage.rows.map((row, index) => (
                          <tr key={`${row.date}-${index}`}>
                            <td><time dateTime={row.date}>{row.date.slice(0, 10)}</time></td>
                            <td>{row.description}</td>
                            <td>
                              {formatSignedCredits(row.amount, numLocale) ?? row.amount} {t("credits")}
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                )}
              </section>
            </>
          )}
        </>
      )}
    </section>
  );
}
