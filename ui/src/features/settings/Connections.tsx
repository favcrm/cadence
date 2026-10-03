import { useEffect, useId, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import { useMaybeResource } from "../../lib/useResource";
import type { Connection, ConnectionProvider } from "../../lib/types";
import Button from "../../ui/Button";
import { ResourceGate } from "../../ui/ResourceStatus";
import { IconRefresh } from "../../ui/icons";
import {
  acceptsSmtp,
  acceptsToken,
  canManage,
  capabilityWords,
  connectionCapabilities,
  enrollmentShapes,
  isAvailable,
  pinWord,
  readinessText,
  scopeHint,
  smtpPortTlsError,
  smtpSummary,
  isSmtpSender,
  smtpUnreadable,
  smtpErrorMessage,
} from "./connectionsView";
import { HOSTED_SMTP_NOTE } from "./emailSendingView";
import { connectionLabel } from "../../lib/connections";

/**
 * CAD-1013 safe enrollment errors. A provider/daemon refusal can carry
 * raw downstream text that echoes the credential the operator just
 * typed (SMTP host/password in a TLS or auth error). The UI never
 * renders `ApiError.message` on this path — it renders a stable, typed
 * `code` mapped to a safe reason + recovery, and falls back to a
 * generic message when the code is absent or unknown. New typed codes
 * land here as the backend (CAD-1014) supplies them; anything else is
 * deliberately non-specific.
 */
const ENROLL_ERROR: Record<string, string> = {
  // The only stable code the daemon actually emits on this path
  // (platform_rpc enroll): custody_unprotected. It can truthfully say
  // nothing was stored — the daemon refused before custody took the
  // credential. Invented codes for non-emitted paths would only mask a
  // future fix under a false specific; the generic fallback below is the
  // honest answer until the backend emits a real code.
  custody_unprotected:
    "The daemon cannot isolate this credential from agents using the same system account, so it refused to store it — nothing was stored. To save it anyway, tick the storage-consent box below, then retry — it is never pre-selected.",
};

/**
 * CAD-1013: the scopes an SMTP sender must enroll — exactly the reviewed
 * send-capability scopes on the provider descriptor, never the union of
 * every provider permission. Falls back to the reviewed-union hint only
 * when no send capability is declared (kept narrow, still reviewed).
 */
function smtpRequiredScopes(provider: ConnectionProvider | null): string[] {
  const caps = provider?.descriptor?.capabilities ?? [];
  // Only the reviewed send-capability scopes — a provider that declares
  // none cannot express an SMTP enrollment, so return empty rather than
  // falling back to the union of every permission it happens to list.
  const seen = new Set<string>();
  for (const cap of caps) {
    if (cap.effect === "send") for (const scope of cap.scopes ?? []) seen.add(scope);
  }
  return [...seen].sort();
}

function enrollErrorMessage(e: unknown): string {
  const err = e instanceof ApiError ? e : null;
  const code = err?.code;
  if (code !== undefined && code in ENROLL_ERROR) return ENROLL_ERROR[code];
  // Unknown or absent code: never surface the raw downstream text — it
  // can echo the credential. The network outcome may be uncertain (the
  // daemon can commit before a lost response), so do not claim 'nothing
  // was saved' — ask the operator to confirm before retrying instead.
  return "Could not confirm the connection was added. Refresh connections before retrying.";
}

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
                            ` · ${enrollmentText(p)}`}
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
export function ConnectionDetail({
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
        {smtpSummary(row) && (
          <div className="flex flex-wrap gap-x-2 min-w-0">
            <dt className="text-ink-500">Sender</dt>
            <dd className="text-ink-200 break-words">{smtpSummary(row)}</dd>
          </div>
        )}
        {row.sender_unusable === "hosted_smtp_unsupported" && (
          <div className="flex flex-wrap gap-x-2 min-w-0">
            <dt className="text-ink-500">Sender</dt>
            <dd className="text-ink-300 break-words" data-hosted-smtp>
              {HOSTED_SMTP_NOTE}
            </dd>
          </div>
        )}
        {!row.sender_unusable && smtpErrorMessage(row) && (
          <div className="flex flex-wrap gap-x-2 min-w-0">
            <dt className="text-ink-500">Sender</dt>
            <dd className="text-fail break-words" role="alert" data-smtp-error={row.smtp_error ?? "unavailable"}>
              {smtpErrorMessage(row)}
            </dd>
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

/** Plain words for one provider's reviewed enrollment shapes. */
function enrollmentText(p: ConnectionProvider): string {
  const shapes = enrollmentShapes(p);
  if (shapes.length === 0) return "no enrollment";
  return shapes
    .map((s) => (s === "smtp" ? "SMTP sender enrollment" : "token enrollment"))
    .join(" + ");
}

/** Replace an enrolled credential. The token clears the moment the request settles. */
export function RotateForm({
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
  // SMTP senders rotate the password under the same identity; the
  // password is primary and the optional server details sit collapsed
  // behind human labels. Blank fields inherit the live custody values;
  // the port/security selector is a PAIRED transport choice — "keep
  // current", or the valid (465 implicit) / (587 STARTTLS) pair — never
  // a raw, independently-editable tls_mode field. SMTP keeps its live
  // scopes (the rotation never narrows or widens them); token providers
  // still expose their own scopes box.
  const isSmtp = isSmtpSender(row);
  // CAD-1064: when the live settings can't be read there is nothing to
  // inherit — the operator re-enters every transport field.
  const unreadable = smtpUnreadable(row);
  // transport: "" = keep the live host/port/tls pairing; otherwise the
  // operator picks a paired submission transport.
  const [smtpTransport, setSmtpTransport] = useState("");
  const [smtp, setSmtp] = useState({ host: "", username: "", sender: "", sender_name: "" });

  const submit = () => {
    if (busy) return;
    const wanted = scopes.split(/[\s,]+/).map((s) => s.trim()).filter(Boolean);
    // SMTP never touches scopes — omitting them preserves the live grant.
    // Token providers only carry a scopes override when one was typed.
    const base = !isSmtp && wanted.length > 0 ? { scopes: wanted } : {};
    // Resolve the effective host/port/tls: a chosen transport pair, else
    // the live custody values, else any typed host.
    const liveHost = row.smtp?.host ?? "";
    const livePort = row.smtp?.port ?? 0;
    const liveTls = row.smtp?.tls_mode ?? "";
    const effHost = smtp.host.trim() !== "" ? smtp.host.trim() : liveHost;
    const effPort = smtpTransport === "" ? livePort : smtpTransport === "implicit" ? 465 : 587;
    const effTls = smtpTransport === "" ? liveTls : smtpTransport;
    const hostChanged = smtp.host.trim() !== "";
    const transportChanged = smtpTransport !== "";
    if (unreadable) {
      const missing =
        smtp.host.trim() === "" ? "server host"
        : smtpTransport === "" ? "port & security"
        : smtp.username.trim() === "" ? "username"
        : smtp.sender.trim() === "" ? "sender address"
        : null;
      if (missing) { setError(`Enter the ${missing} — the saved settings can't be read, so none are kept.`); return; }
    }
    if (isSmtp && (hostChanged || transportChanged)) {
      // Validate the EFFECTIVE inherited host/port/TLS before the request
      // — a changed piece must still form a valid (465 implicit) /
      // (587 starttls) submission against the live or new host.
      const bad = smtpPortTlsError(effHost, String(effPort), effTls);
      if (bad) { setError(bad); return; }
    }
    const body = isSmtp
      ? {
          ...base,
          secret: token.trim(),
          ...(smtp.host.trim() ? { host: smtp.host.trim() } : {}),
          ...(transportChanged ? { port: effPort, tls_mode: effTls } : {}),
          ...(smtp.username.trim() ? { username: smtp.username.trim() } : {}),
          ...(smtp.sender.trim() ? { sender: smtp.sender.trim() } : {}),
          ...(smtp.sender_name.trim() ? { sender_name: smtp.sender_name.trim() } : {}),
        }
      : { ...base, token: token.trim() };
    if (!isSmtp && token.trim() === "") return;
    if (isSmtp && token.trim() === "") {
      setError("Enter the fresh SMTP password — rotation always replaces it.");
      return;
    }
    setBusy(true);
    setError(null);
    api
      .connectionRotate(row.id, body)
      .then(() => {
        onDone();
        onClose();
      })
      // CAD-1013: rotate also carries the fresh SMTP password — same
      // typed-code safe reason, never the raw downstream message.
      .catch((e: ApiError) => setError(enrollErrorMessage(e)))
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
        {isSmtp && (
          <> Rotating an SMTP sender invalidates its installation bindings until the
          operator rebinds them.</>
        )}
      </p>
      <div>
        <label htmlFor={tokenId} className="text-label font-medium text-ink-200">
          {isSmtp ? "New SMTP password" : "New token"}
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
      {unreadable && smtpErrorMessage(row) && (
        <p className="text-label text-fail break-words" role="alert">{smtpErrorMessage(row)}</p>
      )}
      {isSmtp && (
        <details className="space-y-2" open={unreadable}>
          <summary className="text-label font-medium text-ink-300 cursor-pointer select-none">
            Server &amp; sender details{" "}
            <span className="text-ink-500 font-normal">
              {unreadable ? "(required — enter every field)" : "(optional — blank keeps the current values)"}
            </span>
          </summary>
          <fieldset className="space-y-2 mt-2">
            <legend className="sr-only">SMTP server and sender</legend>
            <p className="text-micro text-ink-500 break-words">
              {unreadable
                ? "Nothing is kept from the unreadable settings. Enter the server, port, username and sender address; the sender name is optional."
                : "Leave everything blank to keep the live server and sender. Only fill a field you intend to change."}
            </p>
            <div>
              <label htmlFor={`${scopesId}-host`} className="text-label text-ink-300">
                Server host
              </label>
              <input
                id={`${scopesId}-host`}
                type="text"
                autoComplete="off"
                spellCheck={false}
                value={smtp.host}
                onChange={(e) => setSmtp((cur) => ({ ...cur, host: e.target.value }))}
                placeholder={row.smtp?.host ?? ""}
                className="field w-full mt-1"
                disabled={busy}
              />
            </div>
            <div>
              <label htmlFor={`${scopesId}-transport`} className="text-label text-ink-300">
                Port &amp; security
              </label>
              <select
                id={`${scopesId}-transport`}
                value={smtpTransport}
                onChange={(e) => setSmtpTransport(e.target.value)}
                className="field w-full mt-1"
                disabled={busy}
              >
                {row.smtp ? (
                  <option value="">Keep current — {row.smtp.port} ({row.smtp.tls_mode === "implicit" ? "implicit TLS" : "STARTTLS"})</option>
                ) : (
                  <option value="">Choose…</option>
                )}
                <option value="implicit">465 — implicit TLS</option>
                <option value="starttls">587 — STARTTLS</option>
              </select>
            </div>
            <div>
              <label htmlFor={`${scopesId}-username`} className="text-label text-ink-300">
                Username
              </label>
              <input
                id={`${scopesId}-username`}
                type="text"
                autoComplete="off"
                spellCheck={false}
                value={smtp.username}
                onChange={(e) => setSmtp((cur) => ({ ...cur, username: e.target.value }))}
                placeholder={row.smtp?.username ?? ""}
                className="field w-full mt-1"
                disabled={busy}
              />
            </div>
            <div>
              <label htmlFor={`${scopesId}-sender`} className="text-label text-ink-300">
                Sender address (verified)
              </label>
              <input
                id={`${scopesId}-sender`}
                type="text"
                autoComplete="off"
                spellCheck={false}
                value={smtp.sender}
                onChange={(e) => setSmtp((cur) => ({ ...cur, sender: e.target.value }))}
                placeholder={row.smtp?.sender ?? ""}
                className="field w-full mt-1"
                disabled={busy}
              />
            </div>
            <div>
              <label htmlFor={`${scopesId}-sender_name`} className="text-label text-ink-300">
                Sender name
              </label>
              <input
                id={`${scopesId}-sender_name`}
                type="text"
                autoComplete="off"
                spellCheck={false}
                value={smtp.sender_name}
                onChange={(e) => setSmtp((cur) => ({ ...cur, sender_name: e.target.value }))}
                placeholder={row.smtp?.sender_name ?? ""}
                className="field w-full mt-1"
                disabled={busy}
              />
            </div>
          </fieldset>
        </details>
      )}
      {/* Scopes only exist for token providers — SMTP rotation preserves
         the live scopes and never exposes a raw scopes box. */}
      {!isSmtp && (
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
      )}
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
export function AddConnection({
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
  const candidates = providers.filter(
    (p) => p.descriptor_available && (acceptsToken(p) || acceptsSmtp(p)),
  );
  // The providers load behind the resource: the selection follows the
  // first candidate until the operator picks one explicitly.
  const [explicit, setExplicit] = useState<string | null>(null);
  const provider = explicit ?? candidates[0]?.provider ?? "";
  const [account, setAccount] = useState("");
  const [scopes, setScopes] = useState("");
  const [token, setToken] = useState("");
  const [acceptRisk, setAcceptRisk] = useState(false);
  const [riskNeeded, setRiskNeeded] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const chosen = providers.find((p) => p.provider === provider) ?? null;
  const hint = chosen ? scopeHint(chosen) : [];
  // SMTP senders enroll typed transport material instead of an
  // opaque token; the shape follows the chosen provider.
  const smtpShape = (chosen && acceptsSmtp(chosen) && !acceptsToken(chosen)) || false;
  const [smtp, setSmtp] = useState({ host: "", port: "465", tls_mode: "implicit", username: "", sender: "", sender_name: "" });

  const submit = () => {
    if (busy) return;
    setRiskNeeded(false);
    const cleanedAccount = account.trim();
    // CAD-1013 SMTP simplification: an SMTP sender enrolls exactly the
    // send capability's reviewed scope — not operator-typed, and never
    // the union of every provider permission. Token providers still take
    // an explicit scope list.
    const wanted = smtpShape
      ? smtpRequiredScopes(chosen)
      : scopes.split(/[\s,]+/).map((s) => s.trim()).filter(Boolean);
    if (smtpShape && wanted.length === 0) {
      // No reviewed send capability on this provider — SMTP enrollment is
      // unsupported here; do not widen the grant to unrelated permissions.
      setError(
        "This provider does not offer SMTP sending — it has no reviewed send permission to enroll.",
      );
      return;
    }
    if (!provider || cleanedAccount === "" || wanted.length === 0) {
      setError(
        smtpShape
          ? "Name this email account and fill in the SMTP details below."
          : "Choose a provider and fill in the account and at least one scope.",
      );
      return;
    }
    if (smtpShape) {
      const port = smtp.port.trim();
      if (
        smtp.host.trim() === "" || port === "" || !/^\d+$/.test(port) ||
        smtp.username.trim() === "" || token.trim() === "" || smtp.sender.trim() === ""
      ) {
        setError("Fill in the host, port, login, password and verified sender for the SMTP sender.");
        return;
      }
      const portTlsError = smtpPortTlsError(smtp.host.trim(), port, smtp.tls_mode);
      if (portTlsError) {
        setError(portTlsError);
        return;
      }
      setBusy(true);
      setError(null);
      api
        .connectionCreate({
          provider,
          account: cleanedAccount,
          shape: "smtp",
          host: smtp.host.trim(),
          port: Number(port),
          tls_mode: smtp.tls_mode,
          username: smtp.username.trim(),
          secret: token.trim(),
          sender: smtp.sender.trim(),
          ...(smtp.sender_name.trim() ? { sender_name: smtp.sender_name.trim() } : {}),
          scopes: wanted,
          ...(acceptRisk ? { accept_same_uid_risk: true } : {}),
        })
        .then((out) => onAdded(out.connection.id))
        // CAD-1013: typed-code safe reason — never the raw downstream
        // message, which can echo the SMTP host/password just typed.
        // custody_unprotected surfaces the risk toggle near Save.
        .catch((e: ApiError) => {
          setRiskNeeded(e instanceof ApiError && e.code === "custody_unprotected");
          setError(enrollErrorMessage(e));
        })
        .finally(() => {
          // The password crossed this one request into daemon
          // custody. It must not survive in the form, whatever the
          // outcome.
          setToken("");
          setBusy(false);
        });
      return;
    }
    if (token.trim() === "") {
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
      // CAD-1013: typed-code safe reason — never raw downstream text.
      .catch((e: ApiError) => {
        setRiskNeeded(e instanceof ApiError && e.code === "custody_unprotected");
        setError(enrollErrorMessage(e));
      })
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
          No provider currently offers enrollment. {existing.length > 0 && (
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
              {smtpShape ? "Email account name" : "Account"}
            </label>
            <input
              id={accountId}
              type="text"
              autoComplete="off"
              spellCheck={false}
              value={account}
              onChange={(e) => setAccount(e.target.value)}
              placeholder={
                provider === "agenticos_external"
                  ? "ws_…"
                  : smtpShape
                    ? "newsletter"
                    : "account name"
              }
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
          {!smtpShape && (
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
          )}
          {smtpShape ? (
            <fieldset className="space-y-2">
              <legend className="text-label font-medium text-ink-200">
                SMTP server — encrypted submission only
              </legend>
              <p className="text-micro text-ink-500 break-words">
                The daemon verifies the server certificate and refuses plaintext, downgrades
                and unverifiable hosts before sending. Choose the security your provider
                expects — port and TLS move together.
              </p>
              <div>
                <label htmlFor={`${tokenId}-host`} className="text-label text-ink-300">
                  Server host
                </label>
                <input
                  id={`${tokenId}-host`}
                  type="text"
                  autoComplete="off"
                  spellCheck={false}
                  value={smtp.host}
                  onChange={(e) => setSmtp((cur) => ({ ...cur, host: e.target.value }))}
                  placeholder="mail.example.com"
                  className="field w-full mt-1"
                  disabled={busy}
                />
              </div>
              <div>
                <label htmlFor={`${tokenId}-tls`} className="text-label text-ink-300">
                  Port &amp; security
                </label>
                <select
                  id={`${tokenId}-tls`}
                  value={smtp.tls_mode}
                  onChange={(e) => setSmtp((cur) => ({
                    ...cur,
                    tls_mode: e.target.value,
                    port: e.target.value === "implicit" ? "465" : "587",
                  }))}
                  className="field w-full mt-1"
                  disabled={busy}
                >
                  <option value="implicit">465 — implicit TLS</option>
                  <option value="starttls">587 — STARTTLS</option>
                </select>
              </div>
              <div>
                <label htmlFor={`${tokenId}-username`} className="text-label text-ink-300">
                  Username
                </label>
                <input
                  id={`${tokenId}-username`}
                  type="text"
                  autoComplete="off"
                  spellCheck={false}
                  value={smtp.username}
                  onChange={(e) => setSmtp((cur) => ({ ...cur, username: e.target.value }))}
                  className="field w-full mt-1"
                  disabled={busy}
                />
              </div>
              <div>
                <label htmlFor={tokenId} className="text-label font-medium text-ink-200">
                  Password
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
                  The password travels inside this one request to daemon custody and is
                  cleared from this form afterwards. Never paste a credential anywhere else
                  on this board.
                </p>
              </div>
              <div>
                <label htmlFor={`${tokenId}-sender`} className="text-label text-ink-300">
                  Sender address (verified)
                </label>
                <input
                  id={`${tokenId}-sender`}
                  type="text"
                  autoComplete="off"
                  spellCheck={false}
                  value={smtp.sender}
                  onChange={(e) => setSmtp((cur) => ({ ...cur, sender: e.target.value }))}
                  placeholder="news@example.com"
                  className="field w-full mt-1"
                  disabled={busy}
                />
              </div>
              <div>
                <label htmlFor={`${tokenId}-sender_name`} className="text-label text-ink-300">
                  Sender name (optional)
                </label>
                <input
                  id={`${tokenId}-sender_name`}
                  type="text"
                  autoComplete="off"
                  spellCheck={false}
                  value={smtp.sender_name}
                  onChange={(e) => setSmtp((cur) => ({ ...cur, sender_name: e.target.value }))}
                  className="field w-full mt-1"
                  disabled={busy}
                />
              </div>
            </fieldset>
          ) : (
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
          )}
          {/* CAD-1013: the storage consent stays collapsed while it is
             optional. When the daemon reports custody_unprotected it
             moves inline above Save so the required consent is visible
             at the point of action — and it is never pre-selected. */}
          {!riskNeeded && (
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
                  This server cannot isolate saved credentials from agents using the same
                  system account. I accept storing this credential here. Leave this off
                  unless you knowingly accept it — it is never pre-selected.
                </span>
              </label>
            </details>
          )}
          {error && (
            <p className="text-label text-fail break-words" role="alert">
              {error}
            </p>
          )}
          {riskNeeded && (
            <label
              className="flex items-start gap-2 text-ink-200 border border-warn/40 rounded px-3 py-2 bg-warn/10"
              data-risk-needed
            >
              <input
                type="checkbox"
                checked={acceptRisk}
                onChange={(e) => setAcceptRisk(e.target.checked)}
                disabled={busy}
                className="mt-0.5"
              />
              <span>
                <strong>Storage consent required:</strong> this server cannot isolate saved
                SMTP passwords from agents using the same system account. I accept storing
                this password here. This is never pre-selected.
              </span>
            </label>
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
