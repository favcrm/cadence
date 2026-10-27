import { useCallback, useEffect, useState } from "react";
import Button from "../../ui/Button";
import { IconRefresh } from "../../ui/icons";
import { sessionHeaders } from "../../lib/sessionKey";
import { privateFetch, withPrivateOperation } from "../../lib/privateSession";
import {
  displayAmount,
  isPlatformAccount,
  safeManageUrl,
  type PlatformAccount as Account,
} from "./accountDisplay";
import "./account.css";
import { useTranslate } from "../../lib/locale";

export default function PlatformAccount() {
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
        </>
      )}
    </section>
  );
}
