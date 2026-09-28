import { useCallback, useEffect, useState } from "react";
import Button from "../../ui/Button";
import { IconRefresh } from "../../ui/icons";
import { sessionHeaders } from "../../lib/sessionKey";
import {
  displayAmount,
  safeManageUrl,
  type PlatformAccount as Account,
} from "./accountDisplay";
import "./account.css";

export default function PlatformAccount() {
  const [data, setData] = useState<Account | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(false);
  const [checkedAt, setCheckedAt] = useState<Date | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(false);
    try {
      const response = await fetch("/api/platform-account", {
        headers: sessionHeaders(),
      });
      if (!response.ok) throw new Error("Account request failed");
      const next = (await response.json()) as Account;
      setData(next);
      setCheckedAt(new Date());
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
          <h1 id="account-title">Account</h1>
          <p>Plan, balance and usage from your connected platform. This page is read-only.</p>
          {checkedAt && (
            <p className="account-checked">
              Last checked <time dateTime={checkedAt.toISOString()}>{checkedAt.toLocaleString()}</time>
            </p>
          )}
        </div>
        <div className="account-actions">
          <Button icon={<IconRefresh />} loading={loading} onClick={() => void refresh()}>
            Refresh
          </Button>
          {manage && data?.configured && (
            <a className="btn btn-primary" href={manage} target="_blank" rel="noopener noreferrer">
              Manage account<span className="sr-only"> (opens in a new tab)</span>
            </a>
          )}
        </div>
      </header>

      {error && (
        <div className="account-notice" data-tone="fail" role="alert">
          <strong>{data ? "Couldn’t refresh account" : "Couldn’t load account"}</strong>
          <p>
            {data
              ? "Showing the information from the last successful check. Refresh to try again."
              : "Account information is unavailable. Refresh to try again."}
          </p>
        </div>
      )}

      {!data && loading && (
        <div className="account-state" role="status">Loading account…</div>
      )}

      {data?.configured === false && (
        <div className="account-state">
          <h2>No connected platform account</h2>
          <p>Connect an account outside this read-only board, then refresh to see its plan and usage.</p>
        </div>
      )}

      {data?.configured && (
        <>
          {account?.company.name && (
            <div className="account-identity">
              <span>Connected account</span>
              <strong>{account.company.name}</strong>
            </div>
          )}
          {account ? (
            <section className="account-summary" aria-label="Account summary">
              <article className="card account-summary-card">
                <h2>Available balance</h2>
                <p className="account-balance">
                  <span>{displayAmount(account.balance.amount)}</span>
                  <span className="account-currency">{currency}</span>
                </p>
                {account.balance.low && (
                  <p className="account-balance-warning" role="status">
                    {account.balance.zero
                      ? "No model credit available."
                      : "Model credit is running low."}
                  </p>
                )}
              </article>
              <article className="card account-summary-card">
                <h2>Plan</h2>
                <p className="account-plan">{account.plan?.name || "No plan"}</p>
                <p className="account-plan-status">
                  {account.plan?.status.replaceAll("_", " ") || "No subscription is configured."}
                </p>
              </article>
            </section>
          ) : (
            <div className="account-notice" role="status">
              <strong>Balance and plan unavailable</strong>
              <p>{data.account_error || "The connected platform did not return an account summary."}</p>
            </div>
          )}

          {account && data.account_error && (
            <div className="account-notice" role="alert">
              <strong>Account details may be incomplete</strong>
              <p>{data.account_error}</p>
            </div>
          )}

          <section className="account-usage" aria-labelledby="account-usage-title">
            <div className="account-section-heading">
              <h2 id="account-usage-title">Recent usage</h2>
              {!data.usage_error && data.usage.length > 0 && (
                <span>{data.usage.length} {data.usage.length === 1 ? "entry" : "entries"}</span>
              )}
            </div>
            {data.usage_error ? (
              <div className="account-state" role="alert">
                <strong>Usage unavailable</strong>
                <p>{data.usage_error}</p>
              </div>
            ) : data.usage.length === 0 ? (
              <div className="account-state">No recent usage to show.</div>
            ) : (
              <div className="card account-usage-table">
                <table>
                  <caption className="sr-only">Recent account usage</caption>
                  <thead>
                    <tr><th scope="col">Date</th><th scope="col">Description</th><th scope="col">Amount</th></tr>
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
            Member information is not available from this read-only connection.
            {manage && " Use Manage account to view and manage members."}
          </p>
        </>
      )}
    </section>
  );
}
