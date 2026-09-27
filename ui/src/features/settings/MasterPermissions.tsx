import { useCallback, useEffect, useState } from "react";
import { api, ApiError, type MasterPermissionRule } from "../../lib/api";

function ruleLine(rule: MasterPermissionRule): string {
  const head = rule.argv.join(" ");
  const tail = rule.tail.length > 0 ? ` ${rule.tail.join(" ")}` : "";
  return `${head}${tail}`;
}

/**
 * Settings → Master permissions (CAD-615). Lists the operator-owned
 * rules in `agents/master/permissions.yaml` with who saved them and
 * when, and revokes one. The daemon refuses anyone but the operator.
 */
export default function MasterPermissions() {
  const [rules, setRules] = useState<MasterPermissionRule[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const load = useCallback(() => {
    api
      .permissionRules()
      .then((out) => {
        setRules(out.rules);
        setError(null);
      })
      .catch((e: ApiError) => setError(e.message ?? String(e)));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const revoke = (id: string) => {
    setBusy(id);
    api
      .permissionRevoke(id)
      .then(() => load())
      .catch((e: ApiError) => setError(e.message ?? String(e)))
      .finally(() => setBusy(null));
  };

  return (
    <main className="px-4 lg:px-8 pt-6 pb-9" data-settings="permissions">
      <h1 className="text-section font-semibold text-ink-100">Master permissions</h1>
      <p className="text-body text-ink-400 mt-2 max-w-2xl">
        Rules the operator saved for the master. A deny wins over an allow. Revoke takes effect on the next check.
      </p>
      {error ? (
        <p className="text-micro text-fail mt-3" role="alert">
          {error}
        </p>
      ) : null}
      {rules === null ? (
        <p className="text-micro text-ink-500 mt-4">Loading…</p>
      ) : rules.length === 0 ? (
        <p className="text-micro text-ink-500 mt-4">No permission rules.</p>
      ) : (
        <ul className="mt-4 space-y-2 max-w-3xl">
          {rules.map((rule) => (
            <li key={rule.id} className="rounded border border-ink-800 px-3 py-2" data-rule={rule.id}>
              <div className="flex items-center gap-2 min-w-0">
                <span className="chip shrink-0 bg-ink-800 text-ink-300">{rule.effect}</span>
                <span className="chip shrink-0 bg-ink-800 text-ink-400">{rule.scope}</span>
                <span className="min-w-0 flex-1 truncate text-secondary text-ink-200" title={ruleLine(rule)}>
                  {ruleLine(rule)}
                </span>
                <button
                  type="button"
                  className="lnk text-label shrink-0"
                  disabled={busy === rule.id}
                  onClick={() => revoke(rule.id)}
                >
                  {busy === rule.id ? "Revoking…" : "Revoke"}
                </button>
              </div>
              <p className="text-micro text-ink-500 mt-1">
                {rule.by} · {new Date(rule.at * 1000).toISOString()}
              </p>
            </li>
          ))}
        </ul>
      )}
    </main>
  );
}
