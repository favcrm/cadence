import { useCallback, useEffect, useState } from "react";
import Button from "../../ui/Button";
import { IconRefresh } from "../../ui/icons";
import { sessionHeaders } from "../../lib/sessionKey";
import { privateFetch, withPrivateOperation } from "../../lib/privateSession";
import {
  displayAmount,
  formatApproxHkdFromMinor,
  formatApproxHkdFromScaled,
  formatUsagePercent,
  formatUsdFromMinor,
  formatUsdFromScaled,
  isPlatformAccount,
  packCapacityLabel,
  parseCheckoutUrlResponse,
  parseHostedBilling,
  planCapacityLabel,
  readBillingReturnParams,
  resolveUsageSummary,
  safeManageUrl,
  safeStripeCheckoutUrl,
  type HostedBillingOfferPlan,
  type HostedBillingSnapshot,
  type HostedBillingUsageRow,
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
  const [usageTab, setUsageTab] = useState<"daily" | "log" | "events" | null>(null);
  const [selectedDay, setSelectedDay] = useState<string | null>(null);
  const [hoverDay, setHoverDay] = useState<string | null>(null);
  const [dailyPage, setDailyPage] = useState(1);
  const [logPage, setLogPage] = useState(1);
  const [extraUsageRows, setExtraUsageRows] = useState<HostedBillingUsageRow[]>([]);
  const [extraUsageCursor, setExtraUsageCursor] = useState<string | null | undefined>(undefined);
  const [loadingMoreUsage, setLoadingMoreUsage] = useState(false);

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
        setExtraUsageRows([]);
        setExtraUsageCursor(undefined);
        setLogPage(1);
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
  const summary = account ? resolveUsageSummary(account, offer) : null;
  const isExtraPrimary = summary?.mode === "overflow" || summary?.mode === "refill_only";
  const dailyList = data?.usage.dailyUsage ?? [];
  const activeTab: "daily" | "log" | "events" =
    usageTab ?? (dailyList.length > 0 ? "daily" : "log");

  const combinedLogRows = [
    ...(data?.usage.rows ?? []),
    ...extraUsageRows,
  ];
  const currentCursor =
    extraUsageCursor !== undefined ? extraUsageCursor : (data?.usage.cursor ?? null);

  const loadOlderUsagePage = useCallback(async () => {
    if (!currentCursor || loadingMoreUsage) return;
    setLoadingMoreUsage(true);
    try {
      await withPrivateOperation(async (op) => {
        const response = await privateFetch(
          `/api/account/billing?cursor=${encodeURIComponent(currentCursor)}`,
          { headers: sessionHeaders() },
          op,
        );
        op.assertCurrent();
        if (!response.ok) return;
        const payload: unknown = await response.json();
        op.assertCurrent();
        const parsed = parseHostedBilling(payload);
        if (!parsed) return;
        setExtraUsageRows((prev) => [...prev, ...parsed.usage.rows]);
        setExtraUsageCursor(parsed.usage.cursor);
        setLogPage((p) => p + 1);
      });
    } catch {
      /* keep existing loaded rows visible */
    } finally {
      setLoadingMoreUsage(false);
    }
  }, [currentCursor, loadingMoreUsage]);

  return (
    <section className="account-workspace" aria-labelledby="account-title" aria-busy={loading}>
      <header className="account-heading">
        <div>
          <h1 id="account-title">{t("Billing")}</h1>
          <p>{t("Manage monthly usage, extra usage packs, plans and usage activity for this workspace.")}</p>
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
              : t("No charge was made. You can pick a plan or extra usage pack anytime.")}
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

          {(!data.canManage || !account || !summary) ? (
            <div className="account-state" data-testid="billing-member-readonly">
              <h2>{t("Billing is managed by workspace owners.")}</h2>
              <p>{t("Only workspace owners can view capacity, change plans, or purchase extra usage packs.")}</p>
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

              {summary.mode === "overflow" && (
                <div className="account-notice billing-notice-info billing-notice-row" role="status" data-testid="billing-overflow-banner">
                  <div>
                    <strong>{t("Monthly allowance reached — Extra usage active")}</strong>
                    <p>{t("Requests are now drawing automatically from your prepaid extra usage pool.")}</p>
                  </div>
                  {allPlans.length > 0 && (
                    <Button
                      size="sm"
                      variant="primary"
                      onClick={() => document.getElementById("billing-plans")?.scrollIntoView({ behavior: "smooth" })}
                    >
                      {t("Upgrade plan")}
                    </Button>
                  )}
                </div>
              )}

              {summary.mode === "depleted" && (
                <div className="account-notice billing-notice-row" data-tone="fail" role="status" data-testid="billing-depleted-banner">
                  <div>
                    <strong>{t("Monthly allowance reached — agent runs paused")}</strong>
                    <p>{t("Capacity is paused until your cycle resets or you upgrade or add an extra usage pack.")}</p>
                  </div>
                  <div className="billing-banner-actions">
                    {allPlans.length > 0 && (
                      <Button
                        size="sm"
                        onClick={() => document.getElementById("billing-plans")?.scrollIntoView({ behavior: "smooth" })}
                      >
                        {t("Upgrade plan")}
                      </Button>
                    )}
                    {account.packs.length > 0 && (
                      <Button
                        size="sm"
                        variant="primary"
                        onClick={() => document.getElementById("billing-packs")?.scrollIntoView({ behavior: "smooth" })}
                      >
                        {t("Add extra usage")}
                      </Button>
                    )}
                  </div>
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
                <article className="card account-summary-card" data-testid="billing-usage-summary">
                  {summary.mode === "overflow" && (
                    <div className="billing-completed-cycle" data-testid="billing-completed-cycle">
                      <div className="billing-completed-row">
                        <span>{t("Monthly allowance · 100% used")}</span>
                        {summary.cycle?.resetsAt && (
                          <span>
                            {t("Resets {date}").replace("{date}", new Date(summary.cycle.resetsAt).toLocaleDateString())}
                          </span>
                        )}
                      </div>
                      <div className="billing-completed-track" aria-hidden="true">
                        <div className="billing-completed-fill" />
                      </div>
                    </div>
                  )}

                  <div className="billing-card-head">
                    <div className="billing-head-left">
                      <h2>
                        {isExtraPrimary
                          ? t("Extra usage")
                          : summary.mode === "trial"
                            ? t("Trial usage")
                            : summary.mode === "depleted"
                              ? t("Monthly allowance reached")
                              : t("Monthly usage")}
                      </h2>
                      {isExtraPrimary && (
                        <span className="billing-status-pill" data-tone="info">{t("Active")}</span>
                      )}
                    </div>
                    {account.packs.length > 0 && (
                      <Button
                        size="sm"
                        onClick={() => document.getElementById("billing-packs")?.scrollIntoView({ behavior: "smooth" })}
                      >
                        {t("Add extra usage")}
                      </Button>
                    )}
                  </div>

                  {(() => {
                    const shownPct = isExtraPrimary
                      ? summary.extra.poolUsedPercent
                      : (summary.cycle?.usedPercent ?? 100);
                    const remPct = isExtraPrimary
                      ? summary.extra.poolRemainingPercent
                      : (summary.cycle?.remainingPercent ?? 0);
                    const barTone =
                      summary.mode === "depleted"
                        ? "fail"
                        : isExtraPrimary
                          ? "info"
                          : shownPct >= 80
                            ? "warn"
                            : "ok";
                    return (
                      <>
                        <div className="billing-usage-row">
                          <p className="account-balance" data-testid="billing-usage-percent">
                            {shownPct}%
                          </p>
                          <span className="billing-usage-sub">
                            {isExtraPrimary
                              ? t("of extra usage pool used")
                              : summary.mode === "trial"
                                ? t("of trial allowance used")
                                : t("of monthly allowance used")}
                          </span>
                        </div>
                        <div className="billing-usage-track" aria-hidden="true">
                          <div className="billing-usage-fill" data-tone={barTone} style={{ width: `${shownPct}%` }} />
                        </div>
                        <div className="billing-usage-meta">
                          <span>{t("{pct}% remaining").replace("{pct}", String(remPct))}</span>
                          {isExtraPrimary ? (
                            <span>{t("Prepaid · Never expires · No surprise overage bills")}</span>
                          ) : summary.cycle?.resetsAt ? (
                            <span>
                              {summary.mode === "trial"
                                ? `${t("Trial ends")} ${new Date(summary.cycle.resetsAt).toLocaleDateString()}`
                                : t("Resets {date}").replace("{date}", new Date(summary.cycle.resetsAt).toLocaleDateString())}
                            </span>
                          ) : null}
                        </div>
                      </>
                    );
                  })()}

                  {summary.mode === "monthly_standby" && summary.extra.hasReserve && (
                    <div className="billing-extra-standby" data-testid="billing-extra-standby">
                      <span className="billing-extra-standby-left">
                        <span className="billing-swatch" data-kind="included" aria-hidden="true" />
                        <span>{t("Extra usage")}</span>
                        <span className="billing-status-pill">{t("Standby")}</span>
                      </span>
                      <span>
                        {t("{pct}% left · Applies automatically at 100%").replace(
                          "{pct}",
                          String(summary.extra.poolRemainingPercent),
                        )}
                      </span>
                    </div>
                  )}
                </article>

                <article className="card account-summary-card" data-testid="billing-plan-card">
                  <div className="billing-card-head">
                    <h2>{t("Billing plan")}</h2>
                    <span
                      className="billing-status-pill"
                      data-tone={
                        summary.mode === "depleted"
                          ? "fail"
                          : summary.mode === "overflow"
                            ? "info"
                            : subActive
                              ? "ok"
                              : account.trial?.status === "active"
                                ? "info"
                                : undefined
                      }
                    >
                      {summary.mode === "overflow"
                        ? t("In overflow")
                        : account.plan?.status === "active"
                          ? t("Active")
                          : account.plan?.status === "past_due"
                            ? t("Past due")
                            : account.trial?.status === "active"
                              ? t("Trial active")
                              : summary.mode === "refill_only"
                                ? t("Pay-as-you-go")
                                : t("No active subscription")}
                    </span>
                  </div>
                  <p className="account-plan">
                    {subActive && account.plan
                      ? account.plan.name
                      : account.trial?.status === "active"
                        ? t("Free trial")
                        : t("No active plan")}
                  </p>
                  <p className="account-plan-status">
                    {account.plan?.status === "active"
                      ? t("Active")
                      : account.plan?.status === "past_due"
                        ? t("Past due")
                        : account.trial?.status === "active"
                          ? t("Trial active")
                          : summary.mode === "refill_only"
                            ? t("Pay-as-you-go (Extra usage)")
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
                <section id="billing-plans" className="billing-section" aria-labelledby="billing-plans-title">
                  <div className="account-section-heading">
                    <h2 id="billing-plans-title">{t("Subscription plans")}</h2>
                    {showIntervalToggle && (
                      <div className="billing-interval-toggle" role="group" aria-label={t("Billing interval")}>
                        <button
                          type="button"
                          className="billing-interval-btn"
                          aria-pressed={interval === "month"}
                          onClick={() => setInterval("month")}
                        >
                          {t("Monthly billing")}
                        </button>
                        <button
                          type="button"
                          className="billing-interval-btn"
                          aria-pressed={interval === "year"}
                          onClick={() => setInterval("year")}
                        >
                          {t("Yearly billing")}
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
                      const capacity = planCapacityLabel(allPlans, plan);
                      const actionKey = `sub:${plan.id}`;
                      return (
                        <article
                          key={plan.id}
                          className="card billing-catalog-card"
                          data-current={currentPlan || undefined}
                          data-plan-id={plan.id}
                        >
                          <div>
                            <div className="billing-plan-card-head">
                              <strong>{plan.label}</strong>
                              <span className="billing-multiplier-badge">{capacity}</span>
                            </div>
                            <div className="billing-price-row">
                              <span className="billing-price-usd">{usd}</span>
                              <span className="billing-price-period">
                                {plan.period === "year" ? t("/ yr") : t("/ mo")}
                              </span>
                              {hkd && <span className="billing-price-hkd">{hkd}</span>}
                            </div>
                            <p className="billing-included-credits">
                              {capacity} {t("monthly usage capacity")}
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
                    <h2 id="billing-packs-title">{t("Extra usage packs")}</h2>
                    <span>{t("Prepaid · Never expires · No surprise overage bills")}</span>
                  </div>
                  <div className="billing-catalog-grid">
                    {account.packs.map((pack) => {
                      const usd = formatUsdFromMinor(pack.chargeMinor, numLocale) ?? `${pack.chargeMinor}`;
                      const hkd = isZh ? formatApproxHkdFromMinor(pack.chargeMinor) : null;
                      const capacity = packCapacityLabel(account.packs, allPlans, pack);
                      const actionKey = `pack:${pack.id}`;
                      return (
                        <article key={pack.id} className="card billing-catalog-card" data-pack-id={pack.id}>
                          <div>
                            <div className="billing-plan-card-head">
                              <strong>{pack.label}</strong>
                              <span className="billing-multiplier-badge">{capacity}</span>
                            </div>
                            <div className="billing-price-row">
                              <span className="billing-price-usd">{usd}</span>
                              {hkd && <span className="billing-price-hkd">{hkd}</span>}
                            </div>
                            <p className="billing-included-credits">
                              {t("{capacity} standard monthly capacity").replace("{capacity}", capacity)}
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

              {(() => {
                const dailyReversed = [...dailyList].reverse();
                const filteredDaily = selectedDay
                  ? dailyReversed.filter((d) => d.date === selectedDay)
                  : dailyReversed;
                const dailyPageSize = 7;
                const dailyTotalPages = Math.max(1, Math.ceil(filteredDaily.length / dailyPageSize));
                const safeDailyPage = Math.min(dailyPage, dailyTotalPages);
                const visibleDaily = filteredDaily.slice(
                  (safeDailyPage - 1) * dailyPageSize,
                  safeDailyPage * dailyPageSize,
                );

                const filteredLog = selectedDay
                  ? combinedLogRows.filter((row) => row.date.slice(0, 10) === selectedDay)
                  : combinedLogRows;
                const logPageSize = 8;
                const loadedLogPages = Math.max(1, Math.ceil(filteredLog.length / logPageSize));
                const safeLogPage = Math.min(logPage, loadedLogPages);
                const visibleLog = filteredLog.slice(
                  (safeLogPage - 1) * logPageSize,
                  safeLogPage * logPageSize,
                );
                const milestoneRows = combinedLogRows.filter((row) => row.kind !== "charge");

                const sourceLabel = (src: "monthly" | "extra" | "mixed") =>
                  src === "extra"
                    ? t("Extra usage")
                    : src === "mixed"
                      ? t("Monthly + Extra")
                      : t("Monthly plan");

                const maxChartPct = Math.max(0.1, ...dailyList.map((d) => d.totalPercent));
                const inspectDay = hoverDay ?? selectedDay;
                const inspectedObj = inspectDay ? dailyList.find((d) => d.date === inspectDay) : null;
                const totalChartPct = dailyList.reduce((acc, d) => acc + d.totalPercent, 0).toFixed(1);

                return (
                  <section className="account-usage" aria-labelledby="account-usage-title">
                    <div className="account-section-heading">
                      <div className="billing-head-left">
                        <h2 id="account-usage-title">{t("Usage activity")}</h2>
                        {selectedDay && activeTab !== "events" && (
                          <button
                            type="button"
                            className="billing-filter-chip"
                            data-testid="billing-day-filter"
                            onClick={() => {
                              setSelectedDay(null);
                              setDailyPage(1);
                              setLogPage(1);
                            }}
                          >
                            {t("Filtered: {date} ×").replace("{date}", selectedDay)}
                          </button>
                        )}
                      </div>
                      <div className="billing-usage-controls">
                        <div className="billing-interval-toggle" role="group" aria-label={t("Usage activity")}>
                          {(["daily", "log", "events"] as const).map((tabKey) => (
                            <button
                              key={tabKey}
                              type="button"
                              className="billing-interval-btn"
                              aria-pressed={activeTab === tabKey}
                              data-usage-tab={tabKey}
                              onClick={() => {
                                setUsageTab(tabKey);
                                setDailyPage(1);
                                setLogPage(1);
                              }}
                            >
                              {tabKey === "daily"
                                ? t("Daily summary")
                                : tabKey === "log"
                                  ? t("Activity log")
                                  : t("Billing events")}
                            </button>
                          ))}
                        </div>
                      </div>
                    </div>

                    {activeTab !== "events" && dailyList.length > 0 && (
                      <div className="card billing-chart-card" data-testid="billing-usage-chart">
                        <div className="billing-chart-head">
                          <div className="billing-chart-title">
                            <strong>{t("14-day usage: {pct}%").replace("{pct}", totalChartPct)}</strong>
                            <span className="billing-chart-readout">
                              {inspectedObj
                                ? `${inspectedObj.date} · ${t("{count} runs").replace("{count}", String(inspectedObj.runCount))} · ${formatUsagePercent(inspectedObj.totalPercent) ?? "0.0%"}`
                                : t("Click any day bar to filter")}
                            </span>
                          </div>
                          <div className="billing-chart-legend">
                            <span className="billing-legend-item">
                              <span className="billing-swatch" data-kind="included" aria-hidden="true" />
                              {t("Monthly plan")}
                            </span>
                            <span className="billing-legend-item">
                              <span className="billing-swatch" data-kind="refill" aria-hidden="true" />
                              {t("Extra usage")}
                            </span>
                          </div>
                        </div>
                        <div className="billing-bars-grid" role="group" aria-label={t("14-day daily usage")}>
                          {dailyList.map((day) => {
                            const hPct = Math.max(10, Math.round((day.totalPercent / maxChartPct) * 100));
                            const extraH =
                              day.source === "extra"
                                ? hPct
                                : day.source === "mixed"
                                  ? Math.max(5, Math.round(hPct / 2))
                                  : 0;
                            const monthlyH = hPct - extraH;
                            return (
                              <button
                                key={day.date}
                                type="button"
                                className="billing-bar-btn"
                                data-chart-day={day.date}
                                aria-pressed={selectedDay === day.date}
                                title={`${day.date} · ${day.runCount} runs · ${formatUsagePercent(day.totalPercent) ?? "0.0%"}`}
                                onMouseEnter={() => setHoverDay(day.date)}
                                onMouseLeave={() => setHoverDay(null)}
                                onClick={() => {
                                  setSelectedDay((prev) => (prev === day.date ? null : day.date));
                                  setDailyPage(1);
                                  setLogPage(1);
                                }}
                              >
                                <span className="billing-bar-col">
                                  {extraH > 0 && <span className="billing-bar-extra" style={{ height: `${extraH}%` }} />}
                                  {monthlyH > 0 && <span className="billing-bar-monthly" style={{ height: `${monthlyH}%` }} />}
                                </span>
                                <span className="billing-bar-day">{day.date.slice(8)}</span>
                              </button>
                            );
                          })}
                        </div>
                      </div>
                    )}

                    {activeTab === "daily" ? (
                      visibleDaily.length === 0 ? (
                        <div className="account-state">{t("No recent usage to show.")}</div>
                      ) : (
                        <div className="card account-usage-table">
                          <table>
                            <caption className="sr-only">{t("Daily workspace usage summary")}</caption>
                            <thead>
                              <tr>
                                <th scope="col">{t("Date")}</th>
                                <th scope="col">{t("Activity")}</th>
                                <th scope="col">{t("Source")}</th>
                                <th scope="col">{t("% of monthly allowance")}</th>
                              </tr>
                            </thead>
                            <tbody>
                              {visibleDaily.map((day) => {
                                const barW = Math.min(100, Math.round((day.totalPercent / maxChartPct) * 100));
                                return (
                                  <tr key={day.date}>
                                    <td><time dateTime={day.date}>{day.date}</time></td>
                                    <td>
                                      <div className="billing-activity-cell">
                                        <div className="billing-activity-top">
                                          <span>{t("{count} runs").replace("{count}", String(day.runCount))}</span>
                                          <button
                                            type="button"
                                            className="billing-view-runs"
                                            onClick={() => {
                                              setSelectedDay(day.date);
                                              setUsageTab("log");
                                              setLogPage(1);
                                            }}
                                          >
                                            {t("View runs")}
                                          </button>
                                        </div>
                                        {day.models.length > 0 && (
                                          <div className="billing-model-chips">
                                            {day.models.map((m) => (
                                              <span key={m} className="billing-model-chip">{m}</span>
                                            ))}
                                          </div>
                                        )}
                                      </div>
                                    </td>
                                    <td>
                                      <span className="billing-source-pill" data-source={day.source}>
                                        {sourceLabel(day.source)}
                                      </span>
                                    </td>
                                    <td>
                                      <span className="billing-share-cell">
                                        <span className="billing-inline-track" aria-hidden="true">
                                          <span className="billing-inline-fill" data-source={day.source} style={{ width: `${barW}%` }} />
                                        </span>
                                        <span>{formatUsagePercent(day.totalPercent) ?? "0.0%"}</span>
                                      </span>
                                    </td>
                                  </tr>
                                );
                              })}
                            </tbody>
                          </table>
                          <div className="billing-pagination" data-testid="billing-pagination">
                            <span>
                              {t("Page {page} of {total}")
                                .replace("{page}", String(safeDailyPage))
                                .replace("{total}", String(dailyTotalPages))}
                            </span>
                            <div className="billing-pagination-controls">
                              <Button
                                size="sm"
                                disabled={safeDailyPage <= 1}
                                onClick={() => setDailyPage((p) => Math.max(1, p - 1))}
                              >
                                {t("Previous")}
                              </Button>
                              <Button
                                size="sm"
                                disabled={safeDailyPage >= dailyTotalPages}
                                onClick={() => setDailyPage((p) => Math.min(dailyTotalPages, p + 1))}
                              >
                                {t("Next")}
                              </Button>
                            </div>
                          </div>
                        </div>
                      )
                    ) : activeTab === "events" ? (
                      milestoneRows.length === 0 ? (
                        <div className="account-state">{t("No billing events to show.")}</div>
                      ) : (
                        <div className="card account-usage-table">
                          <table>
                            <caption className="sr-only">{t("Billing milestones")}</caption>
                            <thead>
                              <tr>
                                <th scope="col">{t("Date")}</th>
                                <th scope="col">{t("Event")}</th>
                                <th scope="col">{t("Source")}</th>
                                <th scope="col">{t("Outcome")}</th>
                              </tr>
                            </thead>
                            <tbody>
                              {milestoneRows.map((row, idx) => {
                                const src = row.source ?? (row.kind === "grant" ? "monthly" : "extra");
                                return (
                                  <tr key={`${row.date}-${idx}`}>
                                    <td><time dateTime={row.date}>{row.date.slice(0, 10)}</time></td>
                                    <td>{row.description}</td>
                                    <td>
                                      <span className="billing-source-pill" data-source={src}>
                                        {sourceLabel(src)}
                                      </span>
                                    </td>
                                    <td>
                                      <span className="billing-status-pill" data-tone={row.kind === "grant" ? "ok" : "info"}>
                                        {row.kind === "credit"
                                          ? t("Refill added")
                                          : row.kind === "grant"
                                            ? t("Cycle reset")
                                            : t("Adjustment")}
                                      </span>
                                    </td>
                                  </tr>
                                );
                              })}
                            </tbody>
                          </table>
                        </div>
                      )
                    ) : visibleLog.length === 0 ? (
                      <div className="account-state">{t("No recent usage to show.")}</div>
                    ) : (
                      <div className="card account-usage-table">
                        <table>
                          <caption className="sr-only">{t("Recent account usage")}</caption>
                          <thead>
                            <tr>
                              <th scope="col">{t("Time")}</th>
                              <th scope="col">{t("Model / Feature")}</th>
                              <th scope="col">{t("Source")}</th>
                              <th scope="col">{t("% of monthly allowance")}</th>
                            </tr>
                          </thead>
                          <tbody>
                            {visibleLog.map((row, index) => {
                              const share =
                                row.kind === "charge"
                                  ? (formatUsagePercent(row.usagePercent) ?? "—")
                                  : row.kind === "credit"
                                    ? t("Refill added")
                                    : row.kind === "grant"
                                      ? t("Cycle reset")
                                      : t("Adjustment");
                              const src = row.source ?? (row.kind === "grant" ? "monthly" : "extra");
                              return (
                                <tr key={`${row.date}-${index}`}>
                                  <td>
                                    <time dateTime={row.date}>
                                      {new Date(row.date).toLocaleString(numLocale, {
                                        month: "short",
                                        day: "numeric",
                                        hour: "2-digit",
                                        minute: "2-digit",
                                      })}
                                    </time>
                                  </td>
                                  <td>{row.model ? `${row.model} · ${row.description}` : row.description}</td>
                                  <td>
                                    <span className="billing-source-pill" data-source={src}>
                                      {sourceLabel(src)}
                                    </span>
                                  </td>
                                  <td>{share}</td>
                                </tr>
                              );
                            })}
                          </tbody>
                        </table>
                        <div className="billing-pagination" data-testid="billing-pagination">
                          <span>
                            {t("Page {page}").replace("{page}", String(safeLogPage))}
                          </span>
                          <div className="billing-pagination-controls">
                            <Button
                              size="sm"
                              disabled={safeLogPage <= 1}
                              onClick={() => setLogPage((p) => Math.max(1, p - 1))}
                            >
                              {t("Previous")}
                            </Button>
                            <Button
                              size="sm"
                              loading={loadingMoreUsage}
                              disabled={safeLogPage >= loadedLogPages && !currentCursor}
                              onClick={() => {
                                if (safeLogPage < loadedLogPages) {
                                  setLogPage((p) => p + 1);
                                } else if (currentCursor) {
                                  void loadOlderUsagePage();
                                }
                              }}
                            >
                              {t("Next")}
                            </Button>
                          </div>
                        </div>
                      </div>
                    )}
                  </section>
                );
              })()}
            </>
          )}
        </>
      )}
    </section>
  );
}
