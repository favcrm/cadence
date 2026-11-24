import {
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from "react";
import Button from "../../ui/Button";
import { IconRefresh } from "../../ui/icons";
import { sessionHeaders } from "../../lib/sessionKey";
import { onPrivateSessionReset, privateFetch, withPrivateOperation } from "../../lib/privateSession";
import {
  displayAmount,
  formatApproxHkdFromMinor,
  formatApproxHkdFromScaled,
  formatUsagePercent,
  formatUsdFromMinor,
  formatUsdFromScaled,
  isPlatformAccount,
  isBillingAttempt,
  packCapacityLabel,
  parseBillingAttempt,
  parseCheckoutUrlResponse,
  parseHostedBilling,
  parseTrackedCheckout,
  planCapacityLabel,
  readBillingReturnParams,
  resolveUsageSummary,
  safeManageUrl,
  safeStripeCheckoutUrl,
  type BillingAttempt,
  type BillingAttemptKind,
  type BillingAttemptState,
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
  sessionScope,
  onRedirect,
}: {
  hosted?: boolean;
  sessionScope?: string | null;
  onBillingSnapshot?: (snapshot: HostedBillingSnapshot | null) => void;
  onRedirect?: (url: string) => void;
} = {}) {
  return hosted ? (
    <HostedBilling onBillingSnapshot={onBillingSnapshot} onRedirect={onRedirect} sessionScope={sessionScope} />
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

function BillingModal({
  title,
  sizeClass,
  testId,
  busy = false,
  closeLabel,
  headerControls,
  onClose,
  children,
}: {
  title: string;
  sizeClass: "billing-modal-plans" | "billing-modal-extra";
  testId: string;
  busy?: boolean;
  closeLabel: string;
  headerControls?: ReactNode;
  onClose: () => void;
  children: ReactNode;
}) {
  const titleId = useId();
  const dialogRef = useRef<HTMLElement | null>(null);
  const openerRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    const active = document.activeElement;
    openerRef.current = active instanceof HTMLElement ? active : null;
    dialogRef.current?.focus();
    return () => {
      const opener = openerRef.current;
      if (opener?.isConnected) opener.focus();
    };
  }, []);

  useEffect(() => {
    const handleEscape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      if (!busy) onClose();
    };
    document.addEventListener("keydown", handleEscape);
    return () => document.removeEventListener("keydown", handleEscape);
  }, [busy, onClose]);

  const focusable = (): HTMLElement[] => {
    const root = dialogRef.current;
    if (!root) return [];
    return Array.from(
      root.querySelectorAll<HTMLElement>(
        'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])',
      ),
    ).filter((el) => el.getAttribute("aria-hidden") !== "true");
  };

  const handleKeyDown = (event: ReactKeyboardEvent<HTMLElement>) => {
    if (event.key !== "Tab") return;
    const items = focusable();
    if (items.length === 0) {
      event.preventDefault();
      dialogRef.current?.focus();
      return;
    }
    const first = items[0]!;
    const last = items[items.length - 1]!;
    const active = document.activeElement;
    if (event.shiftKey) {
      if (active === first || active === dialogRef.current) {
        event.preventDefault();
        last.focus();
      }
    } else if (active === last || active === dialogRef.current) {
      event.preventDefault();
      first.focus();
    }
  };

  return (
    <div className="billing-modal-backdrop" role="presentation" onClick={() => { if (!busy) onClose(); }}>
      <section
        ref={dialogRef}
        tabIndex={-1}
        className={`card billing-modal-panel ${sizeClass}`}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        data-testid={testId}
        onKeyDown={handleKeyDown}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="billing-modal-head">
          <h2 id={titleId}>{title}</h2>
          <div className="billing-modal-head-right">
            {headerControls}
            <button
              type="button"
              className="billing-modal-close"
              aria-label={closeLabel}
              disabled={busy}
              onClick={onClose}
            >
              ×
            </button>
          </div>
        </div>
        <div className="billing-modal-body">{children}</div>
      </section>
    </div>
  );
}

type SavedBillingAttempt = { workspace: string; principal: string; kind: BillingAttemptKind; attemptId: string | null };
const attemptKey = (workspace: string, principal: string, kind: BillingAttemptKind) =>
  `cadence.billing.attempt.${JSON.stringify([workspace, principal, kind])}`;

function saveBillingAttempt(saved: SavedBillingAttempt): boolean {
  try {
    sessionStorage.setItem(attemptKey(saved.workspace, saved.principal, saved.kind), JSON.stringify(saved));
    return true;
  } catch { return false; }
}

function readBillingAttempt(kind: BillingAttemptKind, workspace: string, principal: string): BillingAttempt | "unknown" | "unidentified" | null {
  try {
    const raw = sessionStorage.getItem(attemptKey(workspace, principal, kind));
    if (raw === null) return null;
    const saved: unknown = JSON.parse(raw);
    if (!saved || typeof saved !== "object" || !("workspace" in saved) || !("principal" in saved)
      || !("kind" in saved) || !("attemptId" in saved)
      || saved.workspace !== workspace || saved.principal !== principal || saved.kind !== kind) return "unknown";
    if (saved.attemptId === null) return "unidentified";
    if (!isBillingAttempt(kind, saved.attemptId)) return "unknown";
    return { kind, attemptId: saved.attemptId };
  } catch { return "unknown"; }
}

function HostedBilling({
  onBillingSnapshot,
  onRedirect,
  sessionScope,
}: {
  onBillingSnapshot?: (snapshot: HostedBillingSnapshot | null) => void;
  onRedirect?: (url: string) => void;
  sessionScope?: string | null;
}) {
  const { locale, t } = useLocale();
  const [dataScope, setDataScope] = useState<string | null>(null);
  const [handoffPlan, setHandoffPlan] = useState<string | null>(null);
  const handoffHandled = useRef(false);
  const scopeRef = useRef(sessionScope);
  scopeRef.current = sessionScope;
  const [data, setData] = useState<HostedBillingSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [unsupported, setUnsupported] = useState(false);
  const [error, setError] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [checkedAt, setCheckedAt] = useState<Date | null>(null);
  const [interval, setInterval] = useState<"month" | "year">("month");
  const [activeModal, setActiveModal] = useState<"plans" | "extra" | null>(null);
  const [returnBanner, setReturnBanner] = useState<{ kind: "subscription" | "topup"; status: "success" | "cancel" } | null>(null);
  const [usageTab, setUsageTab] = useState<"daily" | "log" | "events" | null>(null);
  const [selectedDay, setSelectedDay] = useState<string | null>(null);
  const [hoverDay, setHoverDay] = useState<string | null>(null);
  const [dailyPage, setDailyPage] = useState(1);
  const [logPage, setLogPage] = useState(1);
  const [extraUsageRows, setExtraUsageRows] = useState<HostedBillingUsageRow[]>([]);
  const [extraUsageCursor, setExtraUsageCursor] = useState<string | null | undefined>(undefined);
  const [loadingMoreUsage, setLoadingMoreUsage] = useState(false);
  const [attemptStates, setAttemptStates] = useState<Record<BillingAttemptKind, BillingAttemptState | null>>({ subscription: null, topup: null });
  const [checkoutStatus, setCheckoutStatus] = useState<{ attemptId: string; state: BillingAttemptState } | null>(null);
  const [checkingPayment, setCheckingPayment] = useState(false);
  const [purchaseUnsupported, setPurchaseUnsupported] = useState(false);
  const [confirmClose, setConfirmClose] = useState<string | null>(null);
  const actionGuard = useRef(false);
  const refreshVersion = useRef(0);
  const returnHint = useRef(readBillingReturnParams(window.location.search));
  const invalidReturn = useRef(!returnHint.current && (() => {
    const params = new URLSearchParams(window.location.search);
    return params.has("subscription") || params.has("topup");
  })());

  const navigateExternal = useCallback((url: string) => {
    if (onRedirect) onRedirect(url);
    else window.location.assign(url);
  }, [onRedirect]);

  useEffect(() => onPrivateSessionReset(() => {
    refreshVersion.current++;
    setCheckoutStatus(null);
    setCheckingPayment(false);
    setData(null);
    setAttemptStates({ subscription: "unknown", topup: "unknown" });
    onBillingSnapshot?.(null);
  }), [onBillingSnapshot]);

  const savedAttempt = useCallback((kind: BillingAttemptKind, workspace: string, attemptId: string | null) => {
    if (!sessionScope) return false;
    return saveBillingAttempt({ workspace, principal: sessionScope, kind, attemptId });
  }, [sessionScope]);

  const refresh = useCallback(async (opts: { background?: boolean; signal?: AbortSignal } = {}) => {
    if (actionGuard.current) return;
    const version = ++refreshVersion.current;
    const current = () => scopeRef.current === sessionScope && refreshVersion.current === version;
    setCheckingPayment(true);
    if (!opts.background) setLoading(true);
    setError(false);
    try {
      await withPrivateOperation(async (op) => {
        const response = await privateFetch("/api/account/billing", {
          headers: sessionHeaders(),
        }, op);
        op.assertCurrent();
        if (!current()) return;
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
        if (!current()) return;
        if (parsed.role === "owner" && !sessionScope) {
          setData(null);
          onBillingSnapshot?.(null);
          return;
        }
        setUnsupported(false);
        setData(parsed);
        setDataScope(sessionScope ?? null);
        setExtraUsageRows([]);
        setExtraUsageCursor(undefined);
        setLogPage(1);
        setCheckedAt(new Date());
        onBillingSnapshot?.(parsed);

        const states: Record<BillingAttemptKind, BillingAttemptState | null> = { subscription: null, topup: null };
        let checkedCheckout: { attemptId: string; state: BillingAttemptState } | null = null;
        if (parsed.role === "owner" && parsed.canManage && sessionScope) {
          const check = async (attempt: BillingAttempt) => {
            try {
              const query = new URLSearchParams({ kind: attempt.kind, attemptId: attempt.attemptId });
              const result = await privateFetch(`/api/account/billing/attempt?${query}`, { headers: sessionHeaders() }, op);
              const payload: unknown = result.ok ? await result.json() : null;
              op.assertCurrent();
              return parseBillingAttempt(payload, attempt);
            } catch {
              op.assertCurrent();
              return "unknown";
            }
          };
          const checkout = parsed.offer?.checkout;
          const checkoutId = checkout && ["open", "pending", "completed", "unknown"].includes(checkout.state)
            && isBillingAttempt("subscription", checkout.id) ? checkout.id : null;
          for (const kind of ["subscription", "topup"] as const) {
            let attempt = readBillingAttempt(kind, parsed.workspace.id, sessionScope);
            const recover = kind === "subscription" && attempt === "unidentified"
              && !returnHint.current && !invalidReturn.current && checkoutId !== null;
            if (recover && checkoutId !== null) attempt = { kind, attemptId: checkoutId };
            if (attempt && typeof attempt === "object") {
              const state = await check(attempt);
              if (!current()) return;
              if (kind === "subscription" && attempt.attemptId === checkoutId) {
                checkedCheckout = { attemptId: checkoutId, state };
              }
              states[kind] = recover && state !== "unknown"
                && !savedAttempt(kind, parsed.workspace.id, attempt.attemptId) ? "unknown" : state;
            } else if (typeof attempt === "string" || returnHint.current?.kind === kind || invalidReturn.current) {
              states[kind] = "unknown";
            }
          }
          if (checkoutId && !checkedCheckout) {
            const state = await check({ kind: "subscription", attemptId: checkoutId });
            checkedCheckout = { attemptId: checkoutId, state };
          }
        }
        if (!current()) return;
        op.assertCurrent();
        setAttemptStates(states);
        setCheckoutStatus(checkedCheckout);
      }, opts.signal);
    } catch (err) {
      if (err instanceof DOMException && err.name === "AbortError") return;
      if (!current()) return;
      setError(true);
    } finally {
      if (current()) {
        setLoading(false);
        setCheckingPayment(false);
      }
    }
  }, [onBillingSnapshot, savedAttempt, sessionScope]);

  useEffect(() => {
    const abort = new AbortController();
    const returned = readBillingReturnParams(window.location.search);
    if (returned) {
      setReturnBanner(returned);
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
    if (handoffHandled.current || dataScope !== (sessionScope ?? null)) return;
    const params = new URLSearchParams(window.location.search);
    const planId = params.getAll("plan");
    const period = params.getAll("period");
    const plans = data?.offer?.plans ?? (data?.offer?.plan ? [data.offer.plan] : []);
    const match = planId.length === 1 && period.length === 1
      ? plans.find((p) => p.id === planId[0] && p.period === period[0]) : undefined;
    if (match && data?.canManage) {
      handoffHandled.current = true;
      setHandoffPlan(match.id);
      setInterval(match.period);
      setActiveModal("plans");
    }
  }, [data?.workspace.id, data?.offer, data?.canManage, dataScope, sessionScope]);

  const checkout = data?.offer?.checkout;
  const matchingCheckout = checkoutStatus?.attemptId === checkout?.id ? checkoutStatus : null;
  const outstandingCheckout = checkout && ["open", "pending", "completed", "unknown"].includes(checkout.state);
  const unresolvedPayment = Object.values(attemptStates).some((state) => state === "pending" || state === "unknown")
    || (outstandingCheckout && matchingCheckout?.state !== "closed" && matchingCheckout?.state !== "confirmed");

  const runAction = useCallback(async (actionId: string, path: string, body: unknown) => {
    if (actionGuard.current || !data?.canManage || data.role !== "owner" || !sessionScope || dataScope !== sessionScope || unsupported) return;
    const kind: BillingAttemptKind | null = path.endsWith("/subscribe") ? "subscription" : path.endsWith("/topup") ? "topup" : null;
    if (kind && (loading || error || checkingPayment || purchaseUnsupported || unresolvedPayment)) return;
    if (kind === "subscription" && checkout?.available !== true) return;
    const previous = kind ? readBillingAttempt(kind, data.workspace.id, sessionScope) : null;
    if (kind && !savedAttempt(kind, data.workspace.id, null)) {
      setAttemptStates((prev) => ({ ...prev, [kind]: "unknown" }));
      setActionError(t("Checkout status is unknown. Refresh before trying another payment."));
      return;
    }
    actionGuard.current = true;
    if (kind) refreshVersion.current++;
    setBusyAction(actionId);
    setActionError(null);
    if (kind) setAttemptStates((prev) => ({ ...prev, [kind]: "unknown" }));
    try {
      await withPrivateOperation(async (op) => {
        const response = await privateFetch(
          kind ? "/api/account/billing/checkout" : path,
          {
            method: "POST",
            headers: {
              ...sessionHeaders(),
              "Content-Type": "application/json",
              "X-Cadence-Board": "1",
            },
            body: JSON.stringify(kind ? { ...(body as object), kind } : body),
          },
          op,
        );
        const payload: unknown = await response.json().catch(() => null);
        op.assertCurrent();
        if (scopeRef.current !== sessionScope) return;
        if (kind && !response.ok) {
          const unavailable = response.status === 404 || response.status === 501;
          const refused = response.status === 400 && payload && typeof payload === "object"
            && "error" in payload && payload.error && typeof payload.error === "object"
            && "code" in payload.error && payload.error.code === "invalid_request";
          if (unavailable || refused) {
            try {
              if (previous && typeof previous === "object") {
                if (!savedAttempt(kind, data.workspace.id, previous.attemptId)) throw new Error("Storage unavailable");
              } else {
                sessionStorage.removeItem(attemptKey(data.workspace.id, sessionScope, kind));
              }
              setAttemptStates(attemptStates);
            } catch {
              // Keep this attempt blocked when its record could not be retired.
            }
            if (unavailable) setPurchaseUnsupported(true);
            setActionError(t(unavailable ? "Checkout recovery is not available on this board yet." : "The selected checkout option is no longer available. Refresh and choose again."));
            return;
          }
        }
        if (!response.ok) throw new Error("Billing action unavailable");
        if (kind) {
          const result = parseTrackedCheckout(payload, kind);
          if (!result || !savedAttempt(kind, data.workspace.id, result.attempt.attemptId)) throw new Error("Checkout unavailable");
          const ready = result.state === "ready" && result.url;
          setAttemptStates((prev) => ({ ...prev, [kind]: ready ? "pending" : "unknown" }));
          if (ready) navigateExternal(ready);
          else if (result.state === "blocked") setActionError(t("Checkout status is unknown. Refresh before trying another payment."));
          else setActionError(t("Unable to open billing checkout because the redirect link was invalid."));
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
      setActionError(kind
        ? t("Checkout status is unknown. Refresh before trying another payment.")
        : t("Unable to start Stripe checkout. Please try again."));
    } finally {
      actionGuard.current = false;
      setBusyAction(null);
    }
  }, [data, sessionScope, dataScope, error, unsupported, loading, checkingPayment, purchaseUnsupported, unresolvedPayment, checkout?.available, attemptStates, savedAttempt, navigateExternal, t]);

  const openCheckoutUrl = safeStripeCheckoutUrl(data?.offer?.checkout.url ?? null);
  const resumeUrl = openCheckoutUrl?.startsWith("https://checkout.stripe.com/") ? openCheckoutUrl : null;

  const resumeCheckout = useCallback(async () => {
    const checkout = data?.offer?.checkout;
    const id = checkout?.id;
    if (actionGuard.current || !data?.canManage || data.role !== "owner" || !sessionScope
      || dataScope !== sessionScope || error || loading || checkingPayment || unsupported || checkout?.state !== "open"
      || matchingCheckout?.state !== "pending" || !isBillingAttempt("subscription", id)
      || !resumeUrl) return;
    actionGuard.current = true;
    refreshVersion.current++;
    setBusyAction("resume");
    try {
      await withPrivateOperation(async (op) => {
        const query = new URLSearchParams({ kind: "subscription", attemptId: id });
        const response = await privateFetch(`/api/account/billing/attempt?${query}`, { headers: sessionHeaders() }, op);
        const state = parseBillingAttempt(response.ok ? await response.json() : null, { kind: "subscription", attemptId: id });
        op.assertCurrent();
        if (scopeRef.current !== sessionScope) return;
        setCheckoutStatus({ attemptId: id, state });
        if (state !== "pending") return;
        const persisted = savedAttempt("subscription", data.workspace.id, id);
        setAttemptStates((prev) => ({ ...prev, subscription: persisted ? "pending" : "unknown" }));
        if (persisted) navigateExternal(resumeUrl);
      });
    } catch {
      setCheckoutStatus({ attemptId: id, state: "unknown" });
    } finally {
      actionGuard.current = false;
      setBusyAction(null);
    }
  }, [data, sessionScope, dataScope, error, loading, checkingPayment, unsupported, matchingCheckout, savedAttempt, navigateExternal, resumeUrl]);

  const closeCheckout = useCallback(async () => {
    const id = data?.offer?.checkout.id;
    if (actionGuard.current || !data?.canManage || data.role !== "owner" || !sessionScope || dataScope !== sessionScope
      || !isBillingAttempt("subscription", id) || id !== confirmClose || error || loading || checkingPayment || unsupported
      || checkout?.state !== "open") return;
    const stored = readBillingAttempt("subscription", data.workspace.id, sessionScope);
    const trackHere = (stored === null && !returnHint.current && !invalidReturn.current)
      || (typeof stored === "object" && stored?.attemptId === id);
    if (trackHere && !savedAttempt("subscription", data.workspace.id, id)) {
      setCheckoutStatus({ attemptId: id, state: "unknown" });
      return;
    }
    actionGuard.current = true;
    refreshVersion.current++;
    setBusyAction("cancel");
    setCheckoutStatus({ attemptId: id, state: "unknown" });
    if (trackHere) setAttemptStates((prev) => ({ ...prev, subscription: "unknown" }));
    try {
      await withPrivateOperation(async (op) => {
        const response = await privateFetch("/api/account/billing/subscribe/cancel", {
          method: "POST",
          headers: { ...sessionHeaders(), "Content-Type": "application/json", "X-Cadence-Board": "1" },
          body: JSON.stringify({ checkoutId: id }),
        }, op);
        const result: unknown = response.ok ? await response.json() : null;
        op.assertCurrent();
        if (!result || typeof result !== "object" || !("checkoutId" in result) || result.checkoutId !== id
          || !("state" in result) || (result.state !== "cancelled" && result.state !== "completed")) return;
        const query = new URLSearchParams({ kind: "subscription", attemptId: id });
        const check = await privateFetch(`/api/account/billing/attempt?${query}`, { headers: sessionHeaders() }, op);
        const payload: unknown = check.ok ? await check.json() : null;
        op.assertCurrent();
        if (scopeRef.current !== sessionScope) return;
        const state = parseBillingAttempt(payload, { kind: "subscription", attemptId: id });
        setCheckoutStatus({ attemptId: id, state });
        if (trackHere) setAttemptStates((prev) => ({ ...prev, subscription: state }));
      });
    } catch {
      setCheckoutStatus({ attemptId: id, state: "unknown" });
    } finally {
      actionGuard.current = false;
      setBusyAction(null);
      setConfirmClose(null);
      void refresh();
    }
  }, [data, sessionScope, dataScope, confirmClose, error, loading, checkingPayment, unsupported, checkout?.state, savedAttempt, refresh]);

  const openModal = useCallback((kind: "plans" | "extra") => {
    setActionError(null);
    setActiveModal(kind);
  }, []);

  const closeModal = useCallback(() => {
    setActionError(null);
    setActiveModal(null);
  }, []);

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
  const canCloseCheckout = checkout?.state === "open" && isBillingAttempt("subscription", checkout.id)
    && matchingCheckout?.state !== "closed" && matchingCheckout?.state !== "confirmed";
  const canResumeCheckout = canCloseCheckout && matchingCheckout?.state === "pending" && resumeUrl !== null;
  const purchasesBlocked = loading || error || checkingPayment || unsupported || !sessionScope || dataScope !== sessionScope || busyAction !== null;
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
        </div>
      </header>

      {data?.role === "owner" && dataScope === sessionScope && (returnBanner || attemptStates.subscription || attemptStates.topup || checkoutStatus) && (
        <div className="account-notice" role="status">
          {returnBanner?.status === "cancel" && <strong>{t("You left Checkout")}</strong>}
          {returnBanner?.status === "cancel" && <p>{t("Payment may still have been processed.")}</p>}
          {(["subscription", "topup"] as const).map((kind) => {
            const state = attemptStates[kind] ?? (kind === "subscription" && returnBanner?.kind !== kind ? checkoutStatus?.state : null);
            if (!state && returnBanner?.kind !== kind) return null;
            return <p key={kind}>{state === "confirmed"
              ? t("Checkout confirmed. Current access is shown below.")
              : state === "closed" ? t("Checkout closed.")
                : state === "pending" || (!state && checkingPayment) ? t("Confirming payment")
                  : t("Checkout status is unknown. Refresh before trying another payment.")}</p>;
          })}
        </div>
      )}

      {actionError && !activeModal && (
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

      {data && dataScope === (sessionScope ?? null) && (data.role !== "owner" || !!sessionScope) && !unsupported && (
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
                  {allPlans.length > 0 && !isIapManaged && (
                    <Button size="sm" variant="primary" onClick={() => openModal("plans")}>
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
                    {allPlans.length > 0 && !isIapManaged && (
                      <Button size="sm" onClick={() => openModal("plans")}>
                        {t("Choose plan")}
                      </Button>
                    )}
                    {account.packs.length > 0 && (
                      <Button size="sm" variant="primary" onClick={() => openModal("extra")}>
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

              {(() => {
                const planTitle =
                  subActive && account.plan
                    ? account.plan.name
                    : account.trial?.status === "active"
                      ? t("Free trial")
                      : t("No active plan");
                const planUsedPct = summary.cycle
                  ? summary.cycle.usedPercent
                  : summary.mode === "depleted"
                    ? 100
                    : 0;
                const planRemPct = summary.cycle
                  ? summary.cycle.remainingPercent
                  : summary.mode === "depleted"
                    ? 0
                    : 100;
                const planFillTone =
                  summary.mode === "depleted"
                    ? "fail"
                    : summary.mode === "overflow"
                      ? "muted"
                      : planUsedPct >= 80
                        ? "warn"
                        : "ok";
                const planBadge =
                  account.plan?.status === "past_due"
                    ? { label: t("Past due"), tone: "warn" as const }
                    : summary.mode === "overflow"
                      ? { label: t("100% used"), tone: undefined }
                      : summary.mode === "depleted"
                        ? { label: t("100% used"), tone: "fail" as const }
                        : account.plan?.status === "active"
                          ? { label: t("Active"), tone: "ok" as const }
                          : account.trial?.status === "active"
                            ? { label: t("Trial active"), tone: "info" as const }
                            : summary.mode === "refill_only"
                              ? { label: t("Pay-as-you-go"), tone: undefined }
                              : { label: t("No active subscription"), tone: undefined };

                return (
                  <section className="account-summary" aria-label={t("Account summary")}>
                    <article className="card account-summary-card billing-hero-card" data-testid="billing-usage-summary">
                      <div className="billing-card-head" data-testid="billing-plan-card">
                        <div className="billing-hero-title-group">
                          <div className="billing-head-left">
                            <h2 className="billing-hero-heading">{planTitle}</h2>
                            <span className="billing-status-pill" data-tone={planBadge.tone}>
                              {planBadge.label}
                            </span>
                          </div>
                          <p className="billing-hero-sub">
                            {summary.mode === "trial"
                              ? t("Included trial allowance · Upgrade anytime")
                              : !subActive
                                ? t("No active monthly subscription")
                                : t("Included monthly allowance")}
                          </p>
                        </div>
                        {allPlans.length > 0 && (
                          <Button
                            id="billing-open-plans"
                            size="sm"
                            variant={subActive ? "primary" : "secondary"}
                            onClick={() => openModal("plans")}
                          >
                            {subActive ? t("Upgrade") : t("Choose plan")}
                          </Button>
                        )}
                      </div>

                      <div>
                        <div className="billing-usage-row">
                          <p className="account-balance" data-testid="billing-usage-percent">
                            {planUsedPct}%
                          </p>
                          <span className="billing-usage-sub">{t("used")}</span>
                          {summary.cycle?.resetsAt && (
                            <span className="billing-usage-right">
                              {summary.mode === "trial"
                                ? `${t("Trial ends")} ${new Date(summary.cycle.resetsAt).toLocaleDateString()}`
                                : t("Resets {date}").replace("{date}", new Date(summary.cycle.resetsAt).toLocaleDateString())}
                            </span>
                          )}
                        </div>
                        <div className="billing-usage-track" aria-hidden="true">
                          <div className="billing-usage-fill" data-tone={planFillTone} style={{ width: `${planUsedPct}%` }} />
                        </div>
                      </div>

                      <div className="billing-usage-meta">
                        <span>{t("{pct}% remaining").replace("{pct}", String(planRemPct))}</span>
                        {account.trial?.status === "active" ? (
                          <span>
                            {account.trial.tokensUsed.toLocaleString()} / {account.trial.tokenCap.toLocaleString()} {t("tokens")}
                          </span>
                        ) : (
                          <span>{t("Cycle allowance")}</span>
                        )}
                      </div>

                      {canCloseCheckout && (
                        <div className="billing-card-actions">
                          {canResumeCheckout && (
                            <Button size="sm" disabled={purchasesBlocked} loading={busyAction === "resume"} onClick={() => void resumeCheckout()}>
                              {t("Resume checkout")}
                            </Button>
                          )}
                          <Button size="sm" disabled={purchasesBlocked} onClick={() => setConfirmClose(offer?.checkout.id ?? null)}>
                            {t("Close Checkout")}
                          </Button>
                        </div>
                      )}
                    </article>

                    <article className="card account-summary-card billing-hero-card" data-testid="billing-extra-card">
                      <div className="billing-card-head">
                        <div className="billing-hero-title-group">
                          <div className="billing-head-left">
                            <h2 className="billing-hero-heading">{t("Extra usage")}</h2>
                            {isExtraPrimary ? (
                              <span className="billing-status-pill" data-tone="info">{t("Active")}</span>
                            ) : summary.extra.hasReserve ? (
                              <span className="billing-status-pill" data-tone="ok">{t("Standby")}</span>
                            ) : null}
                          </div>
                          <p className="billing-hero-sub">
                            {isExtraPrimary
                              ? t("Active overflow — drawing from prepaid reserve")
                              : summary.extra.hasReserve
                                ? t("Applies automatically when monthly hits 100%")
                                : t("Optional prepaid buffer for overflow at 100%")}
                          </p>
                        </div>
                        {account.packs.length > 0 && (
                          <Button
                            id="billing-open-extra"
                            size="sm"
                            variant={summary.mode === "depleted" ? "primary" : "secondary"}
                            onClick={() => openModal("extra")}
                          >
                            {t("Add extra usage")}
                          </Button>
                        )}
                      </div>

                      {!summary.extra.hasReserve ? (
                        <p className="billing-empty-reserve">
                          {t("No prepaid extra usage added. Add a pack so runs continue automatically if your monthly allowance reaches 100%.")}
                        </p>
                      ) : isExtraPrimary ? (
                        <div>
                          <div className="billing-usage-row">
                            <p className="account-balance" data-testid="billing-extra-percent">
                              {summary.extra.poolUsedPercent}%
                            </p>
                            <span className="billing-usage-sub">{t("of reserve used")}</span>
                            <span className="billing-usage-right">
                              {t("{pct}% remaining").replace("{pct}", String(summary.extra.poolRemainingPercent))}
                            </span>
                          </div>
                          <div className="billing-usage-track" aria-hidden="true">
                            <div
                              className="billing-usage-fill"
                              data-tone="info"
                              style={{ width: `${summary.extra.poolUsedPercent}%` }}
                            />
                          </div>
                        </div>
                      ) : (
                        <div data-testid="billing-extra-standby">
                          <div className="billing-usage-row">
                            <p className="account-balance" data-testid="billing-extra-percent">
                              {summary.extra.poolRemainingPercent}%
                            </p>
                            <span className="billing-usage-sub">{t("left in reserve")}</span>
                            <span className="billing-usage-right">
                              {t("{pct}% used").replace("{pct}", String(summary.extra.poolUsedPercent))}
                            </span>
                          </div>
                          <div className="billing-usage-track" aria-hidden="true">
                            <div
                              className="billing-usage-fill"
                              data-tone="info"
                              style={{ width: `${summary.extra.poolRemainingPercent}%` }}
                            />
                          </div>
                        </div>
                      )}

                      <div className="billing-usage-meta">
                        <span>{t("Prepaid · Never expires")}</span>
                        <span>{t("No surprise overage bills")}</span>
                      </div>
                    </article>
                  </section>
                );
              })()}

              {(subActive || isIapManaged) && (
                <section className="card billing-manage-strip" data-testid="billing-manage-strip">
                  <div>
                    <strong className="billing-manage-title">{t("Invoices & payment details")}</strong>
                    <p className="billing-manage-sub">
                      {isIapManaged
                        ? planProvider === "apple"
                          ? t("Managed via Apple App Store.")
                          : t("Managed via Google Play.")
                        : t("Payment methods, tax details, and invoices are managed in Stripe.")}
                    </p>
                  </div>
                  {!isIapManaged && (
                    <Button
                      size="sm"
                      loading={busyAction === "portal"}
                      disabled={busyAction !== null}
                      onClick={() => void runAction("portal", "/api/account/billing/portal", {})}
                    >
                      {t("Open billing portal")}
                    </Button>
                  )}
                </section>
              )}

              {confirmClose && confirmClose === offer?.checkout.id && canCloseCheckout && (
                <BillingModal
                  title={t("Close Checkout")}
                  sizeClass="billing-modal-extra"
                  testId="billing-close-modal"
                  busy={busyAction === "cancel"}
                  closeLabel={t("Keep Checkout")}
                  onClose={() => setConfirmClose(null)}
                >
                  <p className="billing-manage-sub">{t("Closing Checkout does not cancel a completed payment.")}</p>
                  <div className="billing-card-actions">
                    <Button size="sm" disabled={busyAction !== null} onClick={() => setConfirmClose(null)}>{t("Keep Checkout")}</Button>
                    <Button size="sm" disabled={purchasesBlocked} loading={busyAction === "cancel"} onClick={() => void closeCheckout()}>{t("Confirm close Checkout")}</Button>
                  </div>
                </BillingModal>
              )}

              {activeModal === "plans" && allPlans.length > 0 && (
                <BillingModal
                  title={t("Subscription plans")}
                  sizeClass="billing-modal-plans"
                  testId="billing-plans-modal"
                  busy={busyAction !== null}
                  closeLabel={t("Close")}
                  headerControls={
                    showIntervalToggle ? (
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
                    ) : undefined
                  }
                  onClose={closeModal}
                >
                  {actionError && (
                    <div className="account-notice" data-tone="fail" role="alert">
                      <strong>{t("Billing action failed")}</strong>
                      <p>{actionError}</p>
                    </div>
                  )}
                  {isIapManaged && (
                    <div className="account-notice" role="status">
                      <strong>
                        {planProvider === "apple" ? t("Managed via App Store") : t("Managed via Google Play")}
                      </strong>
                      <p>
                        {planProvider === "apple"
                          ? t("This workspace subscription is billed through the Apple App Store. Manage or cancel it in your Apple ID subscriptions.")
                          : t("This workspace subscription is billed through Google Play. Manage or cancel it in Google Play subscriptions.")}
                      </p>
                    </div>
                  )}
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
                          data-selected={handoffPlan === plan.id || undefined}
                        >
                          <div>
                            <div className="billing-plan-card-head">
                              <strong>{plan.label}</strong>
                              <span className="billing-multiplier-badge">{capacity}</span>
                            </div>
                            {handoffPlan === plan.id && <p>{t("Selected plan")}</p>}
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
                                disabled={purchasesBlocked || purchaseUnsupported || unresolvedPayment || offer?.checkout.available !== true}
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
                  {isZh && (
                    <p className="billing-disclosure">
                      {t("Billed in USD via Stripe. HKD figures are approximate references only.")}
                    </p>
                  )}
                </BillingModal>
              )}

              {activeModal === "extra" && account.packs.length > 0 && (
                <BillingModal
                  title={t("Add extra usage")}
                  sizeClass="billing-modal-extra"
                  testId="billing-extra-modal"
                  busy={busyAction !== null}
                  closeLabel={t("Close")}
                  onClose={closeModal}
                >
                  {actionError && (
                    <div className="account-notice" data-tone="fail" role="alert">
                      <strong>{t("Billing action failed")}</strong>
                      <p>{actionError}</p>
                    </div>
                  )}
                  <div className="billing-extra-reserve-box">
                    <div className="billing-extra-reserve-row">
                      <strong>{t("Current prepaid reserve")}</strong>
                      <span
                        className="billing-status-pill"
                        data-tone={!summary.extra.hasReserve ? undefined : isExtraPrimary ? "info" : "ok"}
                      >
                        {!summary.extra.hasReserve
                          ? t("Not added")
                          : `${t("{pct}% remaining").replace("{pct}", String(summary.extra.poolRemainingPercent))} · ${isExtraPrimary ? t("Active") : t("Standby")}`}
                      </span>
                    </div>
                    {summary.extra.hasReserve && (
                      <div className="billing-usage-track" aria-hidden="true">
                        <div
                          className="billing-usage-fill"
                          data-tone="info"
                          style={{ width: `${summary.extra.poolRemainingPercent}%` }}
                        />
                      </div>
                    )}
                  </div>
                  <div id="billing-packs" className="billing-catalog-grid">
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
                            variant="primary"
                            disabled={!pack.available || purchasesBlocked || purchaseUnsupported || unresolvedPayment}
                            loading={busyAction === actionKey}
                            onClick={() => void runAction(actionKey, "/api/account/billing/topup", { pack: pack.id })}
                          >
                            {pack.available ? t("Buy pack") : t("Unavailable")}
                          </Button>
                        </article>
                      );
                    })}
                  </div>
                  <ul className="billing-explainer-list">
                    <li>{t("Monthly or trial allowance is always used first; extra usage activates automatically at 100%.")}</li>
                    <li>{t("Prepaid · Never expires · No surprise overage bills")}</li>
                  </ul>
                  {isZh && (
                    <p className="billing-disclosure">
                      {t("Billed in USD via Stripe. HKD figures are approximate references only.")}
                    </p>
                  )}
                </BillingModal>
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
