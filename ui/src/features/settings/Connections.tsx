import { useEffect, useId, useRef, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import { useMaybeResource } from "../../lib/useResource";
import type { Connection, ConnectionProvider } from "../../lib/types";
import Button from "../../ui/Button";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import { IconRefresh } from "../../ui/icons";
import PageState from "../../ui/PageState";
import {
  acceptsSmtp,
  acceptsToken,
  canManage,
  capabilityWords,
  connectionCapabilities,
  enrollmentSupport,
  isAvailable,
  pinWord,
  readinessText,
  scopeHint,
  serviceGroups,
  smtpPortTlsError,
  smtpSummary,
  isSmtpSender,
  smtpUnreadable,
  smtpErrorMessage,
  verificationSupport,
  verificationResult,
  type ServiceGroup,
  type VerificationOutcome,
} from "./connectionsView";
import { PRESETS, portFor, type Preset, type TlsMode } from "./hostedSmtpView";
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
  viewer: {
    operator: boolean | null;
    readOnly: boolean;
    /** CAD-1193: splits the `operator: null` unknown into checking
     *  vs a completed-but-unanswered check. */
    access?: "checking" | "unavailable" | null;
    onRetryAccess?: () => void;
  };
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
  // The provider an open Add flow is pinned to, or "" for the pick list;
  // null = closed.
  const [adding, setAdding] = useState<string | null>(null);
  const canWrite = viewer.operator === true && !viewer.readOnly;
  const rows = listState?.data ?? [];
  const providers = providersState?.data ?? [];
  const refresh = () => {
    if (listRes) void listRes.revalidate();
    if (providersRes) void providersRes.revalidate();
  };
  const groups = serviceGroups(providers, rows);
  const addProviders = providers.filter((p) => p.provider !== "local");
  const addBusy = adding !== null;

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
          <div className="flex flex-wrap gap-2 shrink-0 items-center">
            <StaleChip state={listState ?? { data: null, status: "loading", error: null, asOf: null, inFlight: false }} />
            <StaleChip state={providersState ?? { data: null, status: "loading", error: null, asOf: null, inFlight: false }} />
            <Button
              variant="ghost"
              icon={<IconRefresh />}
              loading={Boolean(listState?.inFlight || providersState?.inFlight)}
              onClick={refresh}
            >
              Refresh
            </Button>
            {canWrite && (
              <Button
                variant="primary"
                onClick={() => setAdding("")}
                disabled={addBusy}
              >
                Add connection
              </Button>
            )}
          </div>
        )}
      </div>

      {viewer.operator === null ? (
        <PageState title={viewer.access === "unavailable" ? "Access could not be confirmed" : "Checking access…"}>
          {viewer.access === "unavailable" ? (
            <>
              Connections are available to the operator; the board's access
              check did not answer, so nothing is shown.{" "}
              {viewer.onRetryAccess && (
                <Button size="sm" onClick={viewer.onRetryAccess}>
                  Retry access check
                </Button>
              )}
            </>
          ) : (
            "Connections are available to the operator; the board is still proving this session."
          )}
        </PageState>
      ) : !viewer.operator ? (
        <PageState title="Operator access required">
          Connections are available to the operator. Use Sign in in the status bar if you
          have operator access.
        </PageState>
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
          <ResourceGate
            state={
              providersState ?? { data: null, status: "loading", error: null, asOf: null, inFlight: false }
            }
            loading="loading services…"
            failed="could not load the registered services"
            onRetry={refresh}
          />
          {listState?.data && (
            <div className="space-y-3 min-w-0">
              {adding !== null && (
                <AddConnection
                  // The draft (secret + consent) lives only in this
                  // component; keying on the preset means a different
                  // group Setup can never reuse it.
                  key={adding}
                  providers={addProviders}
                  existing={rows}
                  preset={adding === "" ? null : adding}
                  onClose={() => setAdding(null)}
                  onAdded={(id) => {
                    setAdding(null);
                    setSelected(id);
                    refresh();
                  }}
                />
              )}
              {groups.length === 0 ? (
                <section className="card px-4 py-3.5 min-w-0" aria-label="connections">
                  <p className="text-label text-ink-500">
                    No services are registered and no accounts are connected.
                  </p>
                </section>
              ) : (
                groups.map((group) => (
                  <ServiceSection
                    key={group.provider}
                    group={group}
                    canWrite={canWrite}
                    addBusy={addBusy}
                    selected={selected}
                    providers={providers}
                    onSelect={(id) =>
                      setSelected((cur) => (cur === id ? null : id))
                    }
                    onSetup={() => canWrite && setAdding(group.provider)}
                    onChanged={refresh}
                    onRevoked={() => {
                      setSelected(null);
                      refresh();
                    }}
                  />
                ))
              )}
            </div>
          )}
        </>
      )}
    </section>
  );
}

/**
 * One service group: the provider heading, its account rows and the
 * contextual setup action (or the explicit unsupported state when this
 * service declares no reviewed enrollment).
 */
function ServiceSection({
  group,
  canWrite,
  addBusy,
  selected,
  providers,
  onSelect,
  onSetup,
  onChanged,
  onRevoked,
}: {
  group: ServiceGroup;
  canWrite: boolean;
  addBusy: boolean;
  selected: string | null;
  providers: ConnectionProvider[];
  onSelect: (id: string) => void;
  onSetup: () => void;
  onChanged: () => void;
  onRevoked: () => void;
}) {
  const support = enrollmentSupport(group.info);
  return (
    <section
      className="card px-4 py-3.5 min-w-0"
      aria-label={`${group.label} connections`}
    >
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1 mb-2">
        <h2 className="slabel">{group.label}</h2>
        <span className="text-micro text-ink-500">
          {group.connections.length === 0
            ? "nothing connected"
            : `${group.connections.length} connected`}
        </span>
        {!group.builtin && canWrite && support.shapes.length > 0 && (
          <Button
            variant="ghost"
            size="sm"
            className="ml-auto"
            onClick={onSetup}
            disabled={addBusy}
          >
            Set up {group.label}
          </Button>
        )}
      </div>
      {group.connections.length === 0 ? (
        <p className="text-label text-ink-500 break-words">
          {group.builtin
            ? "No built-in accounts."
            : support.shapes.length === 0
              ? support.reason
              : `No ${group.label} account connected yet.`}
        </p>
      ) : (
        <ul className="space-y-1.5">
          {group.connections.map((row) => (
            <li key={row.id}>
              <button
                type="button"
                onClick={() => onSelect(row.id)}
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
              {selected === row.id && (
                <ConnectionDetail
                  row={row}
                  provider={providers.find((p) => p.provider === row.provider) ?? null}
                  capabilities={connectionCapabilities(providers, row.provider)}
                  canWrite={canWrite}
                  onChanged={onChanged}
                  onRevoked={onRevoked}
                />
              )}
            </li>
          ))}
        </ul>
      )}
      {group.builtin && (
        <p className="text-micro text-ink-500 mt-2 break-words">
          Built-in accounts are always present — they carry no credential and cannot be
          edited or disconnected here.
        </p>
      )}
      {!group.builtin && !group.info && (
        <p className="text-micro text-ink-500 mt-2 break-words">
          This provider&apos;s service metadata could not be loaded — the account stays
          under its recorded name. Refresh services; a failed load is not proof the
          provider was removed.
        </p>
      )}
    </section>
  );
}

/** One connection's detail: metadata, local check, rotate and revoke. */
export function ConnectionDetail({
  row,
  provider,
  capabilities,
  canWrite,
  onChanged,
  onRevoked,
}: {
  row: Connection;
  provider?: ConnectionProvider | null;
  capabilities: string | null;
  canWrite: boolean;
  onChanged: () => void;
  onRevoked: () => void;
}) {
  const [rotating, setRotating] = useState(false);
  const [revoking, setRevoking] = useState(false);
  const [checking, setChecking] = useState(false);
  const [testing, setTesting] = useState(false);
  const [note, setNote] = useState<{ ok: boolean; warn?: boolean; text: string } | null>(null);
  const [verifyNote, setVerifyNote] = useState<VerificationOutcome | null>(null);
  const managed = canManage(row);
  const verify = verificationSupport(provider ?? null);
  // The broker's test is admissible only for a writable operator on an
  // enrolled SMTP sender whose provider advertises the reviewed
  // smtp-login-no-send-v1 operation and whose live row is available — a
  // built-in, a read-only viewer, an unadvertised provider or a
  // manifest-mismatched/unreadable sender never sees an enabled Test.
  const testable =
    canWrite &&
    managed &&
    isSmtpSender(row) &&
    verify.supported &&
    isAvailable(row) &&
    row.status.manifest_status === "matched";
  // Each check/test is bound to a monotonically increasing generation and
  // the exact live context it was issued against — the connection's id,
  // credential revision and registration digest, plus the caller's
  // authority and the provider's advertised support. A rotation,
  // provider re-registration, a registry/support change, a sign-out or a
  // disconnect/selection change produces a new context: it bumps the
  // generation and clears the note so a late response can never
  // overwrite newer state, and a response that does not match the live
  // context is treated as stale, not as evidence about the current
  // account.
  const checkGen = useRef(0);
  const latestCtx = useRef("");
  const context = `${row.id}:${row.revision ?? "x"}:${row.registration_digest ?? "x"}:${provider?.registration_digest ?? "x"}:${canWrite}:${verify.supported}:${isAvailable(row)}:${row.status.manifest_status}`;
  useEffect(() => {
    latestCtx.current = context;
  }, [context]);
  useEffect(() => {
    checkGen.current += 1;
    latestCtx.current = context;
    setNote(null);
    setVerifyNote(null);
    setChecking(false);
    setTesting(false);
  }, [context]);
  useEffect(() => {
    return () => {
      checkGen.current += 1; // unmount invalidates any in-flight check
    };
  }, []);

  const check = () => {
    if (checking) return;
    const gen = ++checkGen.current;
    const want = { id: row.id, revision: row.revision ?? null, registration_digest: row.registration_digest ?? null };
    setChecking(true);
    setNote(null);
    api
      .connectionCheck(row.id)
      .then((out) => {
        if (gen !== checkGen.current) return; // a newer context superseded this
        // No fallback to the request's row: an absent or malformed
        // connection, or one whose identity/revision/registration no
        // longer matches what was checked, is a fixed secret-safe
        // configuration error — never the old row re-shown as healthy.
        const c = out && typeof out === "object" ? out.connection : null;
        const identityMatches =
          c !== null &&
          c.id === want.id &&
          (c.revision ?? null) === want.revision &&
          (c.registration_digest ?? null) === want.registration_digest;
        if (c == null || typeof c !== "object" || !identityMatches) {
          setNote({
            ok: false,
            text: "The check did not return this connection's current state — treat this as a configuration problem, not a connection that works.",
          });
          return;
        }
        const smtpErr = smtpErrorMessage(c);
        if (!isAvailable(c) || smtpErr !== null) {
          setNote({
            ok: false,
            text:
              readinessText(c) +
              (smtpErr ? " " + smtpErr : "") +
              " Fix the configuration, then check again.",
          });
        } else if (c.status.manifest_status !== "matched") {
          setNote({
            ok: false,
            warn: true,
            text:
              pinWord(c.status) +
              " — the local configuration is present but its deployment does not match the reviewed contract, so this is not a verified connection.",
          });
        } else {
          setNote({
            ok: true,
            text: "Configuration available — local inspection only. No provider effect was sent and this proves no upstream connectivity.",
          });
        }
        onChanged();
      })
      .catch(() => {
        if (gen !== checkGen.current) return;
        setNote({
          ok: false,
          text: "The local configuration could not be read — treat this as a configuration problem, not a connection that works.",
        });
      })
      .finally(() => {
        if (gen === checkGen.current) setChecking(false);
      });
  };

  // The approved broker no-send login test (CAD-1065/CAD-1085): it sends
  // only the connection's id and the captured revision+registration
  // digest, and renders only the typed receipt's fixed safe text. A
  // stale/malformed/late response never becomes a positive result.
  const test = () => {
    if (testing) return;
    const gen = ++checkGen.current;
    const ctx = context;
    const want = {
      id: row.id,
      revision: row.revision ?? null,
      registration_digest: row.registration_digest ?? null,
    };
    setTesting(true);
    setVerifyNote(null);
    api
      .connectionTest(row.id, {
        expected_revision: want.revision,
        expected_registration_digest: want.registration_digest,
      })
      .then((out) => {
        if (gen !== checkGen.current || latestCtx.current !== ctx) return;
        const res = verificationResult(out, want);
        if (res === null) {
          setVerifyNote({
            ok: false,
            stale: false,
            checkedAt: null,
            text: "The check did not return a readable result — treat this as a configuration problem, not a connection that works.",
          });
        } else {
          setVerifyNote(res);
          if (res.stale) onChanged(); // a stale result re-reads the live identity
        }
      })
      .catch(() => {
        if (gen !== checkGen.current || latestCtx.current !== ctx) return;
        setVerifyNote({
          ok: false,
          stale: false,
          checkedAt: null,
          text: "The verification could not be run — try again, or check the connection's configuration first.",
        });
      })
      .finally(() => {
        if (gen === checkGen.current) setTesting(false);
      });
  };

  return (
    <section className="card px-4 py-3.5 min-w-0" aria-label={`${connectionLabel(row)} detail`}>
      <h2 className="text-cardtitle font-medium text-ink-100 break-words">
        {connectionLabel(row)}
      </h2>
      <dl className="mt-2 space-y-1 text-label min-w-0">
        <div className="flex flex-wrap gap-x-2">
          <dt className="text-ink-500">Account</dt>
          <dd className="text-ink-200 break-words">
            {row.provider} · {row.account}
          </dd>
        </div>
        {smtpSummary(row) && (
          <div className="flex flex-wrap gap-x-2 min-w-0">
            <dt className="text-ink-500">Sender</dt>
            <dd className="text-ink-200 break-words">{smtpSummary(row)}</dd>
          </div>
        )}
        {smtpErrorMessage(row) && (
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
      </dl>

      <details className="mt-2 text-label min-w-0">
        <summary className="cursor-pointer select-none text-ink-400">
          Technical details
        </summary>
        <dl className="mt-2 space-y-1 min-w-0">
          <div className="flex flex-wrap gap-x-2 min-w-0">
            <dt className="text-ink-500">Connection ID</dt>
            <dd className="num text-ink-200 break-all">{row.id}</dd>
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
          {row.registration_digest && (
            <div className="flex flex-wrap gap-x-2 min-w-0">
              <dt className="text-ink-500">Registration</dt>
              <dd className="num text-ink-200 break-all">{row.registration_digest}</dd>
            </div>
          )}
          {row.status.reviewed_pin && (
            <div className="flex flex-wrap gap-x-2 min-w-0">
              <dt className="text-ink-500">Reviewed pin</dt>
              <dd className="num text-ink-200 break-all">{row.status.reviewed_pin}</dd>
            </div>
          )}
          {row.status.reported_pin && (
            <div className="flex flex-wrap gap-x-2 min-w-0">
              <dt className="text-ink-500">Reported pin</dt>
              <dd className="num text-ink-200 break-all">{row.status.reported_pin}</dd>
            </div>
          )}
          <div className="flex flex-wrap gap-x-2 min-w-0">
            <dt className="text-ink-500">Deployment</dt>
            <dd className="text-ink-200 break-words">{pinWord(row.status)}</dd>
          </div>
          <div className="flex flex-wrap gap-x-2 min-w-0">
            <dt className="text-ink-500">Remote verification</dt>
            <dd className="text-ink-200 break-words">
              {verify.supported
                ? "available"
                : `unsupported — ${verify.reason}`}
            </dd>
          </div>
          <p className="text-micro text-ink-500 pt-1 break-words">
            Listing and local configuration reads never contact this account&apos;s
            network; a remote login test (when offered) is a separate explicit check.
            Either way the result is evidence only — it grants no execution permission
            and proves no message delivery or sender entitlement.
          </p>
        </dl>
      </details>

      {note && (
        <p className={`text-label mt-2 break-words ${note.ok ? "text-ink-300" : note.warn ? "text-warn" : "text-fail"}`} role={note.ok ? "status" : "alert"}>
          {note.text}
        </p>
      )}
      {verifyNote && (
        <p
          className={`text-label mt-2 break-words ${verifyNote.ok ? "text-ink-300" : "text-fail"}`}
          role={verifyNote.ok ? "status" : "alert"}
          data-verify-status={verifyNote.ok ? "success" : verifyNote.stale ? "stale" : "failed"}
        >
          {verifyNote.text}
        </p>
      )}
      {canWrite && (
        <div className="flex flex-wrap items-center gap-2 mt-3">
          <Button variant="secondary" size="sm" onClick={check} loading={checking}>
            {checking ? "Checking…" : "Check configuration"}
          </Button>
          {testable && (
            <Button
              variant="secondary"
              size="sm"
              onClick={test}
              loading={testing}
              disabled={testing || checking}
            >
              {testing ? "Testing…" : "Test connection"}
            </Button>
          )}
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
                Update credentials…
              </Button>
              <Button
                variant="danger"
                size="sm"
                onClick={() => {
                  setRotating(false);
                  setRevoking((v) => !v);
                }}
              >
                Disconnect…
              </Button>
            </>
          )}
        </div>
      )}
      {!canWrite && managed && (
        <p className="text-micro text-ink-500 mt-2">
          Updating credentials and disconnecting are the operator&apos;s on an editable
          board.
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
          Built-in connections are always present — they carry no credential and cannot be
          edited or disconnected through credential management.
        </p>
      )}
    </section>
  );
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
    <div className="mt-3 space-y-2 border-t border-ink-700 pt-3" aria-label={`disconnect ${connectionLabel(row)}`}>
      <p className="text-label text-ink-200 break-words">
        Disconnecting removes this connection&apos;s grants and its bindings and defaults,
        and closes its pending effects. This cannot be undone — re-enrolling the same
        account creates a new connection.
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
          I understand disconnecting {connectionLabel(row)} removes its grants and bindings
          and closes pending effects.
        </span>
      </label>
      {error && (
        <p className="text-label text-fail break-words" role="alert">
          {error}
        </p>
      )}
      <div className="flex flex-wrap gap-2">
        <Button variant="danger" size="sm" onClick={submit} loading={busy} disabled={!armed}>
          {busy ? "Disconnecting…" : "Disconnect"}
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
  preset,
  onClose,
  onAdded,
}: {
  providers: ConnectionProvider[];
  existing: Connection[];
  preset?: string | null;
  onClose: () => void;
  onAdded: (id: string) => void;
}) {
  const providerId = useId();
  const accountId = useId();
  const tokenId = useId();
  const candidates = providers.filter(
    (p) => p.descriptor_available && (acceptsToken(p) || acceptsSmtp(p)),
  );
  // The providers load behind the resource: the canonical selection is
  // captured ONCE at mount — the preset, else the first candidate, else
  // "" (choose-service). It never re-reads a later candidate as a
  // fallback, so a refresh that removes the default can never silently
  // retarget a typed credential at a different provider.
  const [explicit, setExplicit] = useState<string>(preset ?? candidates[0]?.provider ?? "");
  const provider = explicit;
  const [account, setAccount] = useState("");
  const [token, setToken] = useState("");
  const [acceptRisk, setAcceptRisk] = useState(false);
  const [riskNeeded, setRiskNeeded] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const chosen = providers.find((p) => p.provider === provider) ?? null;
  const hint = chosen ? scopeHint(chosen) : [];
  // Token setup is an explicit per-scope choice, never the union.
  const [scopePicks, setScopePicks] = useState<Set<string>>(new Set());
  // When a provider offers both shapes the method is an explicit choice;
  // there is no automatic combination.
  const bothShapes = chosen !== null && acceptsToken(chosen) && acceptsSmtp(chosen);
  const [method, setMethod] = useState<"token" | "smtp" | null>(null);
  const smtpShape =
    chosen !== null &&
    acceptsSmtp(chosen) &&
    (!acceptsToken(chosen) || (bothShapes && method === "smtp"));
  const methodPending = bothShapes && method === null;
  // SMTP senders enroll typed transport material instead of an
  // opaque token; the shape follows the chosen provider.
  const [smtpPreset, setSmtpPreset] = useState<Preset>(PRESETS[0]);
  const [smtp, setSmtp] = useState({
    host: PRESETS[0].host,
    port: "465",
    tls_mode: "implicit" as TlsMode,
    username: "",
    sender: "",
    sender_name: "",
  });

  const resetDraft = () => {
    setToken("");
    setScopePicks(new Set());
    setAccount("");
    setAcceptRisk(false);
    setRiskNeeded(false);
    setError(null);
    setMethod(null);
    setSmtpPreset(PRESETS[0]);
    setSmtp({
      host: PRESETS[0].host,
      port: "465",
      tls_mode: "implicit",
      username: "",
      sender: "",
      sender_name: "",
    });
  };

  // The selection's semantic fingerprint: provider + registration digest
  // + descriptor revision + enrollment shapes. A refresh that changes any
  // of them (registration rolled, descriptor re-reviewed, shapes gained
  // or lost) invalidates the in-flight draft — secret, method, account
  // and consent reset — while the canonical provider stays selected.
  const fingerprint = chosen
    ? `${chosen.provider}:${chosen.registration_digest ?? "x"}:${chosen.descriptor?.revision ?? "x"}:${(chosen.descriptor?.enrollment_shapes ?? []).join(",")}`
    : provider === "" ? "none" : `gone:${provider}`;
  const prevFingerprint = useRef<string | null>(null);
  useEffect(() => {
    if (prevFingerprint.current !== null && prevFingerprint.current !== fingerprint) {
      resetDraft();
    }
    prevFingerprint.current = fingerprint;
  }, [fingerprint]);

  const chooseProvider = (next: string) => {
    setExplicit(next);
    resetDraft();
  };

  const chooseMethod = (next: "token" | "smtp") => {
    // A method change discards the whole prior draft — the SMTP
    // transport/login/sender, the token, the account hint and the
    // consent never carry across.
    resetDraft();
    setMethod(next);
  };

  const choosePreset = (next: Preset) => {
    setSmtpPreset(next);
    // A preset change retargets the SMTP service: the password and the
    // custody consent never carry into a different mail server. The
    // non-secret account/sender fields may stay; transport follows the
    // new preset's paired host/port/TLS.
    setToken("");
    setAcceptRisk(false);
    setRiskNeeded(false);
    setSmtp((cur) => ({ ...cur, host: next.host, tls_mode: next.tls, port: String(next.port) }));
    setError(null);
  };

  const toggleScope = (scope: string) => {
    setScopePicks((cur) => {
      const next = new Set(cur);
      if (next.has(scope)) next.delete(scope);
      else next.add(scope);
      return next;
    });
  };

  const submit = () => {
    if (busy || provider === "" || unavailable || methodPending) return;
    setRiskNeeded(false);
    const cleanedAccount = account.trim();
    // CAD-1013 SMTP simplification: an SMTP sender enrolls exactly the
    // send capability's reviewed scope — not operator-typed, and never
    // the union of every provider permission. Token providers take the
    // operator's explicit per-scope choice, not a typed union.
    const wanted = smtpShape
      ? smtpRequiredScopes(chosen)
      : hint.filter((s) => scopePicks.has(s));
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
          : "Choose a provider, fill in the account and tick at least one permission.",
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
    if (methodPending) {
      setError("Choose how to connect this service — token or SMTP.");
      return;
    }
    if (token.trim() === "") {
      setError("Fill in the account, tick at least one permission and enter the token.");
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

  // provider === "" is the choose-service placeholder; a selected
  // provider that is not currently enrollable (removed from the
  // registry, or its descriptor/shapes gone) is `unavailable`. Either
  // disables submission and never sends a draft.
  const unavailable = provider !== "" && !candidates.some((p) => p.provider === provider);
  const cannotSubmit = busy || provider === "" || unavailable || methodPending;

  return (
    <section className="card px-4 py-3.5 min-w-0" aria-label="add connection">
      <h2 className="text-cardtitle font-medium text-ink-100">
        {chosen ? `Set up ${chosen.provider}` : "Add connection"}
      </h2>
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
              Service
            </label>
            <select
              id={providerId}
              value={provider}
              onChange={(e) => chooseProvider(e.target.value)}
              className="field w-full mt-1"
              disabled={busy}
            >
              {provider === "" && (
                <option value="" disabled>
                  Choose a service…
                </option>
              )}
              {unavailable && (
                <option value={provider}>{provider} — unavailable now</option>
              )}
              {candidates.map((p) => (
                <option key={p.provider} value={p.provider}>
                  {p.provider}
                  {capabilityWords(p) ? ` — ${capabilityWords(p)}` : ""}
                </option>
              ))}
            </select>
            {provider === "" && (
              <p className="text-micro text-ink-500 mt-1 break-words">
                Choose a service to connect.
              </p>
            )}
            {unavailable && (
              <p className="text-micro text-warn mt-1 break-words" role="alert">
                That service is no longer available — pick another service to connect, or
                cancel. Nothing typed here can go to a different service.
              </p>
            )}
          </div>
          {provider === "" || unavailable ? null : (
          <>
          {bothShapes && (
            <div role="group" aria-label="How to connect" className="flex flex-wrap gap-2">
              <Button
                size="sm"
                variant={method === "token" ? "primary" : "secondary"}
                aria-pressed={method === "token"}
                disabled={busy}
                onClick={() => chooseMethod("token")}
              >
                API token
              </Button>
              <Button
                size="sm"
                variant={method === "smtp" ? "primary" : "secondary"}
                aria-pressed={method === "smtp"}
                disabled={busy}
                onClick={() => chooseMethod("smtp")}
              >
                Email (SMTP)
              </Button>
            </div>
          )}
          {methodPending && (
            <p className="text-micro text-ink-500 break-words">
              Choose how to connect {provider} — a scoped API token, or an SMTP sender.
            </p>
          )}
          {!methodPending && (
          <>
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
            <fieldset>
              <legend className="text-label font-medium text-ink-200">
                Permissions <span className="text-ink-500 font-normal">(declare only what the provider granted)</span>
              </legend>
              {hint.length === 0 ? (
                <p className="text-micro text-ink-500 mt-1 break-words">
                  This provider declares no reviewed permission scopes — there is nothing to
                  enroll safely.
                </p>
              ) : (
                <div className="mt-1 space-y-1">
                  {hint.map((s) => (
                    <label key={s} className="flex items-start gap-2 text-label text-ink-300">
                      <input
                        type="checkbox"
                        checked={scopePicks.has(s)}
                        onChange={() => toggleScope(s)}
                        disabled={busy}
                        className="mt-0.5"
                      />
                      <span className="num">{s}</span>
                    </label>
                  ))}
                </div>
              )}
              <p className="text-micro text-ink-500 mt-1 break-words">
                These declarations select which reviewed capabilities the account enrolls —
                they do not grant new upstream scopes.
              </p>
            </fieldset>
          )}
          {smtpShape ? (
            <fieldset className="space-y-2">
              <legend className="text-label font-medium text-ink-200">
                SMTP server — encrypted submission only
              </legend>
              <div role="group" aria-label="Email provider" className="flex flex-wrap gap-2">
                {PRESETS.map((p) => (
                  <Button
                    key={p.id}
                    size="sm"
                    variant={p.id === smtpPreset.id ? "primary" : "secondary"}
                    aria-pressed={p.id === smtpPreset.id}
                    disabled={busy}
                    onClick={() => choosePreset(p)}
                  >
                    {p.label}
                  </Button>
                ))}
              </div>
              <p className="text-micro text-ink-500 break-words" data-hint={smtpPreset.id}>
                {smtpPreset.hint}
              </p>
              <div>
                <label htmlFor={`${tokenId}-username`} className="text-label text-ink-300">
                  Your email address
                </label>
                <input
                  id={`${tokenId}-username`}
                  type="text"
                  autoComplete="off"
                  spellCheck={false}
                  value={smtp.username}
                  onChange={(e) => setSmtp((cur) => ({ ...cur, username: e.target.value }))}
                  placeholder="you@yourcompany.com"
                  className="field w-full mt-1"
                  disabled={busy}
                />
              </div>
              <div>
                <label htmlFor={tokenId} className="text-label font-medium text-ink-200">
                  App password
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
              {smtpPreset.id !== "other" && (
                <p className="text-micro text-ink-500 num break-words" data-server>
                  {smtp.host} · port {portFor(smtp.tls_mode)} ·{" "}
                  {smtp.tls_mode === "implicit" ? "SSL/TLS" : "STARTTLS"}
                </p>
              )}
              {smtpPreset.id === "other" && (
                <>
                  <div>
                    <label htmlFor={`${tokenId}-host`} className="text-label text-ink-300">
                      Mail server
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
                      onChange={(e) =>
                        setSmtp((cur) => ({
                          ...cur,
                          tls_mode: e.target.value as TlsMode,
                          port: String(portFor(e.target.value as TlsMode)),
                        }))
                      }
                      className="field w-full mt-1"
                      disabled={busy}
                    >
                      <option value="implicit">465 — SSL/TLS</option>
                      <option value="starttls">587 — STARTTLS</option>
                    </select>
                  </div>
                </>
              )}
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
          </>
          )}
          </>
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
            <Button variant="primary" size="sm" type="submit" loading={busy} disabled={cannotSubmit}>
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
