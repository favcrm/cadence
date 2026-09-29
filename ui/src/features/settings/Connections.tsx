import { useEffect, useId, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import { useMaybeResource } from "../../lib/useResource";
import type { Connection, ConnectionProvider } from "../../lib/types";
import Button from "../../ui/Button";
import { ResourceGate } from "../../ui/ResourceStatus";
import { IconRefresh } from "../../ui/icons";
import {
  acceptsToken,
  canManage,
  capabilityWords,
  connectionCapabilities,
  isAvailable,
  pinWord,
  readinessText,
  scopeHint,
} from "./connectionsView";
import { connectionLabel } from "../../lib/connections";

/**
 * Settings → Connections (CAD-585): the operator's view of exact
 * provider accounts — the reviewed providers, the always-present Local
 * outbox, enrolled credentials and their local configuration check,
 * rotation and revocation.
 *
 * Reads and writes go through the existing strict HTTP APIs
 * (`/api/connection-providers`, `/api/connections…`), operator-only
 * like the daemon RPC they relay. A token exists only in the immediate
 * form state of the create/rotate request that carries it — it is
 * cleared the moment the request settles and is never stored, logged
 * or rendered.
 */
export default function Connections({
  viewer,
}: {
  viewer: { operator: boolean; readOnly: boolean };
}) {
  const listRes = viewer.operator ? resources.connections : null;
  const providersRes = viewer.operator ? resources.connectionProviders : null;
  const listState = useMaybeResource(listRes);
  const providersState = useMaybeResource(providersRes);
  useEffect(() => {
    if (listRes) void listRes.revalidate();
  }, [listRes]);
  useEffect(() => {
    if (providersRes) void providersRes.revalidate();
  }, [providersRes]);
  const [selected, setSelected] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const canWrite = viewer.operator && !viewer.readOnly;
  const rows = listState?.data ?? [];
  const providers = providersState?.data ?? [];
  const current = rows.find((r) => r.id === selected) ?? null;
  const refresh = () => {
    if (listRes) void listRes.revalidate();
    if (providersRes) void providersRes.revalidate();
  };

  return (
    <section className="px-4 lg:px-8 py-5 min-w-0" aria-labelledby="connections-title">
      <div className="flex flex-wrap items-start gap-3 mb-4">
        <div className="min-w-0 flex-1">
          <h1 id="connections-title" className="text-section font-medium text-ink-100">
            Connections
          </h1>
          <p className="text-label text-ink-400 mt-1 break-words">
            Exact provider accounts in this workspace. Creating or selecting a connection grants
            no worker, app or run permissions.
          </p>
        </div>
        {viewer.operator && (
          <div className="flex flex-wrap gap-2 shrink-0">
            <Button
              variant="ghost"
              icon={<IconRefresh />}
              loading={Boolean(listState?.inFlight || providersState?.inFlight)}
              onClick={refresh}
            >
              Refresh
            </Button>
            {canWrite && (
              <Button variant="primary" onClick={() => setAdding(true)} disabled={adding}>
                Add connection
              </Button>
            )}
          </div>
        )}
      </div>

      {!viewer.operator ? (
        <div className="card px-4 py-5">
          <h2 className="text-secondary font-medium text-ink-100 mb-1">
            Sign in to manage connections
          </h2>
          <p className="text-label text-ink-400">
            Connections are available to the operator. Use Sign in in the top bar to connect
            with a login link.
          </p>
        </div>
      ) : (
        <>
          <ResourceGate
            state={
              listState ?? { data: null, status: "loading", error: null, asOf: null, inFlight: false }
            }
            loading="loading connections…"
            failed="could not load connections"
            onRetry={refresh}
          />
          {listState?.data && providersState?.data && (
            <div className="space-y-3 min-w-0">
              {adding && (
                <AddConnection
                  providers={providers}
                  existing={rows}
                  onClose={() => setAdding(false)}
                  onAdded={(id) => {
                    setAdding(false);
                    setSelected(id);
                    refresh();
                  }}
                />
              )}
              <section className="card px-4 py-3.5 min-w-0" aria-label="connections">
                <h2 className="slabel mb-2">Connections</h2>
                {rows.length === 0 ? (
                  <p className="text-label text-ink-500">No connections.</p>
                ) : (
                  <ul className="space-y-1.5">
                    {rows.map((row) => (
                      <li key={row.id}>
                        <button
                          type="button"
                          onClick={() => setSelected((cur) => (cur === row.id ? null : row.id))}
                          aria-pressed={selected === row.id}
                          aria-label={`${connectionLabel(row)} — ${readinessText(row)}`}
                          className={`w-full text-left px-3 py-2 rounded border min-w-0 ${
                            selected === row.id
                              ? "border-accent/60 bg-accent/10"
                              : "border-ink-700 bg-ink-850 hover:border-edge-hover"
                          }`}
                        >
                          <span className="flex flex-wrap items-center gap-2 min-w-0">
                            <span className="text-label font-medium text-ink-100 min-w-0 break-words flex-1">
                              {connectionLabel(row)}
                            </span>
                            <span
                              className={`chip ${row.kind === "builtin" ? "bg-ink-800 text-ink-300" : "bg-accent/15 text-accent"}`}
                            >
                              {row.kind === "builtin" ? "built-in" : "enrolled"}
                            </span>
                            {!isAvailable(row) && (
                              <span className="chip bg-warn/10 text-warn">unavailable</span>
                            )}
                          </span>
                          <span className="block text-micro text-ink-500 mt-0.5 break-words">
                            {readinessText(row)}
                          </span>
                        </button>
                      </li>
                    ))}
                  </ul>
                )}
              </section>
              {current && (
                <ConnectionDetail
                  key={current.id}
                  row={current}
                  capabilities={connectionCapabilities(providers, current.provider)}
                  canWrite={canWrite}
                  onChanged={refresh}
                  onRevoked={() => {
                    setSelected(null);
                    refresh();
                  }}
                />
              )}
              <section className="card px-4 py-3.5 min-w-0" aria-label="providers">
                <h2 className="slabel mb-2">Providers</h2>
                {providers.length === 0 ? (
                  <p className="text-label text-ink-500">No providers registered.</p>
                ) : (
                  <ul className="space-y-2">
                    {providers.map((p) => (
                      <li key={p.provider} className="text-label min-w-0">
                        <span className="text-ink-100 font-medium">{p.provider}</span>
                        <span className="text-ink-500">
                          {" — "}
                          {!p.descriptor_available
                            ? "no reviewed descriptor — unavailable"
                            : (capabilityWords(p) ?? "no reviewed capabilities")}
                          {p.descriptor_available &&
                            ` · ${acceptsToken(p) ? "token enrollment" : "no enrollment"}`}
                        </span>
                        <span className="block text-micro text-ink-500 mt-0.5 break-words">
                          {pinWord({ manifest_status: p.manifest_status })} ·{" "}
                          {p.network_checked
                            ? "network checked"
                            : "never checked over the network"}
                        </span>
                      </li>
                    ))}
                  </ul>
                )}
                <p className="text-micro text-ink-500 mt-3 break-words">
                  A declaration is not proof that the deployed service holds the reviewed
                  contract. Capability names describe compatible purposes; each provider keeps
                  its exact reviewed tools, scopes and effect classification.
                </p>
              </section>
            </div>
          )}
        </>
      )}
    </section>
  );
}

/** One connection's detail: metadata, local check, rotate and revoke. */
function ConnectionDetail({
  row,
  capabilities,
  canWrite,
  onChanged,
  onRevoked,
}: {
  row: Connection;
  capabilities: string | null;
  canWrite: boolean;
  onChanged: () => void;
  onRevoked: () => void;
}) {
  const [rotating, setRotating] = useState(false);
  const [revoking, setRevoking] = useState(false);
  const [checking, setChecking] = useState(false);
  const [note, setNote] = useState<{ ok: boolean; text: string } | null>(null);
  const managed = canManage(row);

  const check = () => {
    if (checking) return;
    setChecking(true);
    setNote(null);
    api
      .connectionCheck(row.id)
      .then(() => {
        setNote({
          ok: true,
          text: "Configuration checked — local inspection only. No provider effect was sent and this proves no upstream connectivity.",
        });
        onChanged();
      })
      .catch((e: ApiError) => setNote({ ok: false, text: e.message ?? String(e) }))
      .finally(() => setChecking(false));
  };

  return (
    <section className="card px-4 py-3.5 min-w-0" aria-label={`${connectionLabel(row)} detail`}>
      <h2 className="text-cardtitle font-medium text-ink-100 break-words">
        {connectionLabel(row)}
      </h2>
      <dl className="mt-2 space-y-1 text-label min-w-0">
        <div className="flex flex-wrap gap-x-2 min-w-0">
          <dt className="text-ink-500">ID</dt>
          <dd className="num text-ink-200 break-all">{row.id}</dd>
        </div>
        <div className="flex flex-wrap gap-x-2">
          <dt className="text-ink-500">Account</dt>
          <dd className="text-ink-200 break-words">
            {row.provider} · {row.account}
          </dd>
        </div>
        <div className="flex flex-wrap gap-x-2">
          <dt className="text-ink-500">Credential</dt>
          <dd className="text-ink-200">
            {row.kind === "builtin"
              ? "none — built-in"
              : `revision ${row.revision ?? "?"}`}
          </dd>
        </div>
        {row.scopes.length > 0 && (
          <div className="flex flex-wrap gap-x-2 min-w-0">
            <dt className="text-ink-500">Scopes</dt>
            <dd className="num text-ink-200 break-words">{row.scopes.join(", ")}</dd>
          </div>
        )}
        {capabilities && (
          <div className="flex flex-wrap gap-x-2 min-w-0">
            <dt className="text-ink-500">Capabilities</dt>
            <dd className="text-ink-200 break-words">{capabilities}</dd>
          </div>
        )}
        <div className="flex flex-wrap gap-x-2 min-w-0">
          <dt className="text-ink-500">Deployment</dt>
          <dd className="text-ink-200 break-words">{pinWord(row.status)}</dd>
        </div>
      </dl>
      {note && (
        <p className={`text-label mt-2 break-words ${note.ok ? "text-ink-300" : "text-fail"}`} role={note.ok ? "status" : "alert"}>
          {note.text}
        </p>
      )}
      {canWrite && (
        <div className="flex flex-wrap items-center gap-2 mt-3">
          <Button variant="secondary" size="sm" onClick={check} loading={checking}>
            {checking ? "Checking…" : "Check configuration"}
          </Button>
          {managed && (
            <>
              <Button
                variant="secondary"
                size="sm"
                onClick={() => {
                  setRevoking(false);
                  setRotating((v) => !v);
                }}
              >
                Rotate…
              </Button>
              <Button
                variant="danger"
                size="sm"
                onClick={() => {
                  setRotating(false);
                  setRevoking((v) => !v);
                }}
              >
                Revoke…
              </Button>
            </>
          )}
        </div>
      )}
      {!canWrite && managed && (
        <p className="text-micro text-ink-500 mt-2">
          Rotation and revocation are the operator&apos;s on an editable board.
        </p>
      )}
      {managed && rotating && (
        <RotateForm row={row} onDone={onChanged} onClose={() => setRotating(false)} />
      )}
      {managed && revoking && (
        <RevokeConfirm row={row} onDone={onRevoked} onClose={() => setRevoking(false)} />
      )}
      {!managed && (
        <p className="text-micro text-ink-500 mt-2 break-words">
          Built-in connections are always present — they cannot be rotated or revoked through
          credential management.
        </p>
      )}
    </section>
  );
}

/** Replace an enrolled credential. The token clears the moment the request settles. */
function RotateForm({
  row,
  onDone,
  onClose,
}: {
  row: Connection;
  onDone: () => void;
  onClose: () => void;
}) {
  const tokenId = useId();
  const scopesId = useId();
  const [token, setToken] = useState("");
  const [scopes, setScopes] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = () => {
    if (busy || token.trim() === "") return;
    setBusy(true);
    setError(null);
    const wanted = scopes.split(/[\s,]+/).map((s) => s.trim()).filter(Boolean);
    api
      .connectionRotate(row.id, {
        token: token.trim(),
        ...(wanted.length > 0 ? { scopes: wanted } : {}),
      })
      .then(() => {
        onDone();
        onClose();
      })
      .catch((e: ApiError) => setError(e.message ?? String(e)))
      .finally(() => {
        // The credential crossed this one request into daemon custody.
        // It must not survive in the form, whatever the outcome.
        setToken("");
        setBusy(false);
      });
  };

  return (
    <form
      className="mt-3 space-y-2 border-t border-ink-700 pt-3"
      aria-label={`rotate ${connectionLabel(row)}`}
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      <p className="text-label text-ink-300 break-words">
        Replace the credential. The connection keeps its identity and moves to a new revision;
        re-enrolling the same account later creates a new connection instead.
      </p>
      <div>
        <label htmlFor={tokenId} className="text-label font-medium text-ink-200">
          New token
        </label>
        <input
          id={tokenId}
          type="password"
          autoComplete="off"
          spellCheck={false}
          value={token}
          onChange={(e) => setToken(e.target.value)}
          className="field w-full mt-1"
          disabled={busy}
        />
      </div>
      <div>
        <label htmlFor={scopesId} className="text-label font-medium text-ink-200">
          Scopes <span className="text-ink-500 font-normal">(optional — blank keeps the current scopes)</span>
        </label>
        <input
          id={scopesId}
          type="text"
          autoComplete="off"
          spellCheck={false}
          value={scopes}
          onChange={(e) => setScopes(e.target.value)}
          placeholder={row.scopes.join(", ")}
          className="field w-full mt-1"
          disabled={busy}
        />
      </div>
      {error && (
        <p className="text-label text-fail break-words" role="alert">
          {error}
        </p>
      )}
      <div className="flex flex-wrap gap-2">
        <Button variant="primary" size="sm" type="submit" loading={busy} disabled={token.trim() === ""}>
          {busy ? "Replacing…" : "Replace credential"}
        </Button>
        <Button variant="ghost" size="sm" onClick={onClose} disabled={busy}>
          Cancel
        </Button>
      </div>
    </form>
  );
}

/** Revoke with an explicit confirmation — nothing happens on the first click. */
function RevokeConfirm({
  row,
  onDone,
  onClose,
}: {
  row: Connection;
  onDone: () => void;
  onClose: () => void;
}) {
  const [armed, setArmed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = () => {
    if (busy || !armed) return;
    setBusy(true);
    setError(null);
    api
      .connectionRevoke(row.id)
      .then(() => onDone())
      .catch((e: ApiError) => setError(e.message ?? String(e)))
      .finally(() => setBusy(false));
  };

  return (
    <div className="mt-3 space-y-2 border-t border-ink-700 pt-3" aria-label={`revoke ${connectionLabel(row)}`}>
      <p className="text-label text-ink-200 break-words">
        Revoking removes this connection&apos;s grants and defaults and closes its pending
        effects. This cannot be undone — re-enrolling the same account creates a new
        connection.
      </p>
      <label className="flex items-start gap-2 text-label text-ink-300">
        <input
          type="checkbox"
          checked={armed}
          onChange={(e) => setArmed(e.target.checked)}
          disabled={busy}
          className="mt-0.5"
        />
        <span>
          I understand revoking {connectionLabel(row)} removes its grants and closes pending
          effects.
        </span>
      </label>
      {error && (
        <p className="text-label text-fail break-words" role="alert">
          {error}
        </p>
      )}
      <div className="flex flex-wrap gap-2">
        <Button variant="danger" size="sm" onClick={submit} loading={busy} disabled={!armed}>
          {busy ? "Revoking…" : "Revoke connection"}
        </Button>
        <Button variant="ghost" size="sm" onClick={onClose} disabled={busy}>
          Cancel
        </Button>
      </div>
    </div>
  );
}

/** Enroll a scoped token for a supported provider. The token clears on settle. */
function AddConnection({
  providers,
  existing,
  onClose,
  onAdded,
}: {
  providers: ConnectionProvider[];
  existing: Connection[];
  onClose: () => void;
  onAdded: (id: string) => void;
}) {
  const providerId = useId();
  const accountId = useId();
  const scopesId = useId();
  const tokenId = useId();
  const candidates = providers.filter((p) => p.descriptor_available && acceptsToken(p));
  // The providers load behind the resource: the selection follows the
  // first candidate until the operator picks one explicitly.
  const [explicit, setExplicit] = useState<string | null>(null);
  const provider = explicit ?? candidates[0]?.provider ?? "";
  const [account, setAccount] = useState("");
  const [scopes, setScopes] = useState("");
  const [token, setToken] = useState("");
  const [acceptRisk, setAcceptRisk] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const chosen = providers.find((p) => p.provider === provider) ?? null;
  const hint = chosen ? scopeHint(chosen) : [];

  const submit = () => {
    if (busy) return;
    const cleanedAccount = account.trim();
    const wanted = scopes.split(/[\s,]+/).map((s) => s.trim()).filter(Boolean);
    if (!provider || cleanedAccount === "" || wanted.length === 0 || token.trim() === "") {
      setError("Choose a provider and fill in the account, at least one scope and the token.");
      return;
    }
    setBusy(true);
    setError(null);
    api
      .connectionCreate({
        provider,
        account: cleanedAccount,
        shape: "token",
        token: token.trim(),
        scopes: wanted,
        ...(acceptRisk ? { accept_same_uid_risk: true } : {}),
      })
      .then((out) => onAdded(out.connection.id))
      .catch((e: ApiError) => setError(e.message ?? String(e)))
      .finally(() => {
        // The credential crossed this one request into daemon custody.
        // It must not survive in the form, whatever the outcome.
        setToken("");
        setBusy(false);
      });
  };

  return (
    <section className="card px-4 py-3.5 min-w-0" aria-label="add connection">
      <h2 className="text-cardtitle font-medium text-ink-100">Add connection</h2>
      {candidates.length === 0 ? (
        <p className="text-label text-ink-400 mt-2 break-words">
          No provider currently offers token enrollment. {existing.length > 0 && (
            <>Existing connections are listed below.</>
          )}
        </p>
      ) : (
        <form
          className="mt-2 space-y-2"
          onSubmit={(e) => {
            e.preventDefault();
            submit();
          }}
        >
          <div>
            <label htmlFor={providerId} className="text-label font-medium text-ink-200">
              Provider
            </label>
            <select
              id={providerId}
              value={provider}
              onChange={(e) => setExplicit(e.target.value)}
              className="field w-full mt-1"
              disabled={busy}
            >
              {candidates.map((p) => (
                <option key={p.provider} value={p.provider}>
                  {p.provider}
                  {capabilityWords(p) ? ` — ${capabilityWords(p)}` : ""}
                </option>
              ))}
            </select>
          </div>
          <div>
            <label htmlFor={accountId} className="text-label font-medium text-ink-200">
              Account
            </label>
            <input
              id={accountId}
              type="text"
              autoComplete="off"
              spellCheck={false}
              value={account}
              onChange={(e) => setAccount(e.target.value)}
              placeholder={provider === "agenticos_external" ? "ws_…" : "account name"}
              className="field w-full mt-1"
              disabled={busy}
            />
            {provider === "agenticos_external" && (
              <p className="text-micro text-ink-500 mt-1 break-words">
                The exact workspace ID from the device exchange — <span className="num">ws_</span>
                followed by the lowercase UUID.
              </p>
            )}
          </div>
          <div>
            <label htmlFor={scopesId} className="text-label font-medium text-ink-200">
              Scopes
            </label>
            <input
              id={scopesId}
              type="text"
              autoComplete="off"
              spellCheck={false}
              value={scopes}
              onChange={(e) => setScopes(e.target.value)}
              placeholder={hint.length > 0 ? hint.join(", ") : "scope names"}
              className="field w-full mt-1"
              disabled={busy}
            />
            {hint.length > 0 && (
              <p className="text-micro text-ink-500 mt-1 break-words">
                Reviewed scopes for this provider: <span className="num">{hint.join(", ")}</span>.
                Declare only the scopes granted at the provider&apos;s consent screen.
              </p>
            )}
          </div>
          <div>
            <label htmlFor={tokenId} className="text-label font-medium text-ink-200">
              Token
            </label>
            <input
              id={tokenId}
              type="password"
              autoComplete="off"
              spellCheck={false}
              value={token}
              onChange={(e) => setToken(e.target.value)}
              className="field w-full mt-1"
              disabled={busy}
            />
            <p className="text-micro text-ink-500 mt-1 break-words">
              The token travels inside this one request to daemon custody and is cleared from
              this form afterwards. Never paste a credential anywhere else on this board.
            </p>
          </div>
          <details className="text-label">
            <summary className="cursor-pointer select-none text-ink-400">
              Advanced: custody risk acceptance
            </summary>
            <label className="flex items-start gap-2 text-ink-300 mt-2">
              <input
                type="checkbox"
                checked={acceptRisk}
                onChange={(e) => setAcceptRisk(e.target.checked)}
                disabled={busy}
                className="mt-0.5"
              />
              <span>
                Accept the existing same-user custody risk, when the daemon reports the
                enrollment as unprotected. Only choose this when you accept that existing
                risk.
              </span>
            </label>
          </details>
          {error && (
            <p className="text-label text-fail break-words" role="alert">
              {error}
            </p>
          )}
          <div className="flex flex-wrap gap-2">
            <Button variant="primary" size="sm" type="submit" loading={busy}>
              {busy ? "Adding…" : "Add connection"}
            </Button>
            <Button variant="ghost" size="sm" onClick={onClose} disabled={busy}>
              Cancel
            </Button>
          </div>
        </form>
      )}
      {candidates.length === 0 && (
        <div className="flex flex-wrap gap-2 mt-2">
          <Button variant="ghost" size="sm" onClick={onClose}>
            Close
          </Button>
        </div>
      )}
    </section>
  );
}
