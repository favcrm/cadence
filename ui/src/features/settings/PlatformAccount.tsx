import { useCallback, useEffect, useState } from "react";
import Button from "../../ui/Button";
import { sessionHeaders } from "../../lib/sessionKey";
import { displayAmount, safeManageUrl, type PlatformAccount as Account } from "./platformAccount";

export default function PlatformAccount() {
  const [data, setData] = useState<Account | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [updated, setUpdated] = useState<string | null>(null);
  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const response = await fetch("/api/platform-account", { headers: sessionHeaders() });
      if (!response.ok) throw new Error("Account information is unavailable. Try again.");
      setData(await response.json() as Account);
      setUpdated(new Date().toLocaleTimeString());
    } catch {
      setError("Account information is unavailable. Try again.");
    } finally {
      setLoading(false);
    }
  }, []);
  useEffect(() => { void refresh(); }, [refresh]);
  const manage = safeManageUrl(data?.manage_url ?? null);

  if (!loading && data?.configured === false) return <section className="page"><h1>Account</h1><p className="dim">No connected platform account.</p></section>;
  return <section className="page platform-account" aria-busy={loading}>
    <header className="platform-account-header">
      <div><p className="eyebrow">Connected platform</p><h1>{data?.account?.company.name || "Plan & usage"}</h1><p className="dim">Account information is read-only here.</p>{updated && <p className="dim">Last checked {updated}</p>}</div>
      <div className="platform-account-actions">
        <Button onClick={() => void refresh()} loading={loading}>Refresh</Button>
        {manage && <a className="btn btn-primary" href={manage} target="_blank" rel="noopener noreferrer">Manage account ↗</a>}
      </div>
    </header>
    {loading && !data && <p role="status" className="dim">Loading account…</p>}
    {(error || data?.account_error) && <p role="alert" className="platform-account-error">{error || data?.account_error}</p>}
    {data?.account && <div className="platform-account-summary">
      <article><h2>Available balance</h2><p className="platform-account-balance">{displayAmount(data.account.balance.amount)} <span>{data.account.balance.currency}</span></p>{data.account.balance.low && <p className="platform-account-error">{data.account.balance.zero ? "No model credit available." : "Model credit is running low."}</p>}</article>
      <article><h2>Plan</h2><p className="platform-account-plan">{data.account.plan?.name || "No plan"}</p><p className="dim">{data.account.plan?.status.replaceAll("_", " ") || "No subscription is configured."}</p></article>
    </div>}
    {data && <><h2>Recent usage</h2>
      {data.usage_error ? <p role="alert" className="platform-account-error">{data.usage_error}</p> : data.usage.length === 0 ? <p className="dim">No recent usage.</p> : <div className="platform-account-table"><table><thead><tr><th>Date</th><th>Description</th><th>Amount</th></tr></thead><tbody>{data.usage.map((row, index) => <tr key={`${row.date}-${index}`}><td><time dateTime={row.date}>{row.date.slice(0, 10)}</time></td><td>{row.description}</td><td>{displayAmount(row.amount)} {data.account?.balance.currency || "HKD"}</td></tr>)}</tbody></table></div>}
      <h2>Members</h2><p className="dim">Member information is not available from the connected platform’s read-only API.{manage && " Open Manage account to view and manage members."}</p>
    </>}
  </section>;
}
