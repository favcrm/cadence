import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import { useMaybeResource } from "../../lib/useResource";
import { routePath, type ConnectionPage } from "../../lib/router";
import { navigate } from "../../lib/useLocation";
import type { Connection, ConnectionProvider } from "../../lib/types";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import { IconRefresh, IconWarning } from "../../ui/icons";
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
  serviceLabel,
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
import "./connections.css";

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

/** The add/setup/detail routes under the connections section. */
function connectionsIndexHref(): string {
  return routePath({ screen: "settings", section: "connections" });
}
function connectionAddHref(): string {
  return routePath({ screen: "settings", section: "connections", page: { kind: "add" } });
}
function connectionAddProviderHref(provider: string): string {
  return routePath({
    screen: "settings",
    section: "connections",
    page: { kind: "addProvider", provider },
  });
}
function connectionDetailHref(id: string): string {
  return routePath({
    screen: "settings",
    section: "connections",
    page: { kind: "detail", id },
  });
}

const emptyGate = {
  data: null,
  status: "loading" as const,
  error: null,
  asOf: null,
  inFlight: false,
};

/**
 * Settings → Connections (CAD-585, CAD-1319): the operator's view of
 * exact provider accounts — the reviewed providers, the always-present
 * Local outbox, enrolled credentials and their local configuration
 * check, rotation and revocation.
 *
 * Presentation is a small routed family, all under the existing
 * screen=settings / section=connections route object so the sidebar
 * and section tabs stay selected:
 *
 *   /settings/connections                  index of services + accounts
 *   /settings/connections/new              the service chooser
 *   /settings/connections/new/<provider>   one service's pinned setup
 *   /settings/connections/account/<id>     one connection's detail
 *
 * Reads and writes go through the existing strict HTTP APIs
 * (`/api/connection-providers`, `/api/connections…`), operator-only
 * like the daemon RPC they relay. A token exists only in the immediate
 * form state of the create/rotate request that carries it — it is
 * cleared the moment the request settles and is never stored, logged
 * or rendered. A non-operator or a read-only board never mounts the
 * form or the guarded detail actions — the fetch guards below mean no
 * resource request is even issued for them.
 */
export default function Connections({
  viewer,
  page,
}: {
  viewer: {
    operator: boolean | null;
    readOnly: boolean;
    /** CAD-1193: splits the `operator: null` unknown into checking
     *  vs a completed-but-unanswered check. */
    access?: "checking" | "unavailable" | null;
    onRetryAccess?: () => void;
  };
  page?: ConnectionPage;
}) {
  const canWrite = viewer.operator === true && !viewer.readOnly;
  // Unknown access grants nothing on any Connections route. Reuse the
  // index's checking/unavailable state until the bounded proof answers.
  if (viewer.operator === null) {
    return <ConnectionsIndex viewer={viewer} canWrite={false} />;
  }
  const provenViewer = { ...viewer, operator: viewer.operator };
  if (page?.kind === "add" || page?.kind === "addProvider") {
    return <ConnectionsAdd viewer={provenViewer} canWrite={canWrite} page={page} />;
  }
  if (page?.kind === "detail") {
    // Keyed to the id: an id change remounts the page so a stale
    // per-mount `answered` flag can never carry the proof across ids.
    return <ConnectionsDetail key={page.id} viewer={provenViewer} canWrite={canWrite} id={page.id} />;
  }
  return <ConnectionsIndex viewer={viewer} canWrite={canWrite} />;
}

/**
 * The connections index: every registered service and enrolled account,
 * grouped by provider. Accounts are compact links to their own detail
 * pages — nothing expands in place.
 */
function ConnectionsIndex({
  viewer,
  canWrite,
}: {
  viewer: {
    operator: boolean | null;
    readOnly: boolean;
    access?: "checking" | "unavailable" | null;
    onRetryAccess?: () => void;
  };
  canWrite: boolean;
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
  const rows = listState?.data ?? [];
  const providers = providersState?.data ?? [];
  const refresh = () => {
    if (listRes) void listRes.revalidate();
    if (providersRes) void providersRes.revalidate();
  };
  const groups = serviceGroups(providers, rows);

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
            <StaleChip state={listState ?? emptyGate} />
            <StaleChip state={providersState ?? emptyGate} />
            <Button
              variant="ghost"
              icon={<IconRefresh />}
              loading={Boolean(listState?.inFlight || providersState?.inFlight)}
              onClick={refresh}
            >
              Refresh
            </Button>
            {canWrite && (
              <Button variant="primary" href={connectionAddHref()}>
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
            state={listState ?? emptyGate}
            loading="loading connections…"
            failed="could not load connections"
            onRetry={refresh}
          />
          <ResourceGate
            state={providersState ?? emptyGate}
            loading="loading services…"
            failed="could not load the registered services"
            onRetry={refresh}
          />
          {listState?.data && (
            <div className="space-y-3 min-w-0">
              {groups.length === 0 ? (
                <section className="card px-4 py-3.5 min-w-0" aria-label="connections">
                  <p className="text-label text-ink-500">
                    No services are registered and no accounts are connected.
                  </p>
                  {canWrite && (
                    <p className="text-label mt-2">
                      <Link href={connectionAddHref()} className="lnk">
                        Add a connection
                      </Link>
                    </p>
                  )}
                </section>
              ) : (
                groups.map((group) => (
                  <ServiceSection
                    key={group.provider}
                    group={group}
                    canWrite={canWrite}
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
 * One service group on the index: the provider heading, its account
 * links and a setup action when the service offers a reviewed
 * enrollment. Never silently drops an unavailable provider or an
 * orphaned account.
 */
function ServiceSection({
  group,
  canWrite,
}: {
  group: ServiceGroup;
  canWrite: boolean;
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
        {group.builtin && (
          <span className="chip bg-ink-800 text-ink-300">built-in</span>
        )}
        {!group.builtin && canWrite && support.shapes.length > 0 && (
          <Button
            variant="ghost"
            size="sm"
            className="ml-auto"
            href={connectionAddProviderHref(group.provider)}
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
              <Link
                href={connectionDetailHref(row.id)}
                className="conn-row"
              >
                <span className="conn-row-main">
                  <span className="text-label font-medium text-ink-100 min-w-0 break-words">
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
                <span className="conn-row-sub text-micro text-ink-500 break-words">
                  {readinessText(row)}
                </span>
              </Link>
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

/**
 * The dedicated Add pages (CAD-1319): `/new` is the backend-driven
 * service chooser; `/new/<provider>` is that one service's pinned
 * setup. A non-operator or a read-only board gets the shared blocked
 * state and no resource requests or forms — the guards below are the
 * same as the index's.
 */
function ConnectionsAdd({
  viewer,
  canWrite,
  page,
}: {
  viewer: { operator: boolean; readOnly: boolean };
  canWrite: boolean;
  page: ConnectionPage & { kind: "add" | "addProvider" };
}) {
  const providersRes = viewer.operator ? resources.connectionProviders : null;
  const listRes = viewer.operator ? resources.connections : null;
  const providersState = useMaybeResource(providersRes);
  const listState = useMaybeResource(listRes);
  useEffect(() => {
    if (providersRes) void providersRes.revalidate();
  }, [providersRes]);
  useEffect(() => {
    if (listRes) void listRes.revalidate();
  }, [listRes]);
  const providers = providersState?.data ?? [];
  const rows = listState?.data ?? [];
  const refresh = () => {
    if (providersRes) void providersRes.revalidate();
    // The list is re-read after an enrollment: bypass the freshness
    // window so a just-created row is not reported missing.
    if (listRes) void listRes.invalidate();
  };
  // The chooser/setup candidates: the reviewed enrollment shapes only.
  // Provider "local" (the built-in outbox) is never offered for setup.
  const addProviders = providers.filter((p) => p.provider !== "local");
  const pinned = page.kind === "addProvider" ? page.provider : null;
  const chosen = pinned ? addProviders.find((p) => p.provider === pinned) ?? null : null;

  let title = "Add connection";
  if (page.kind === "addProvider") title = chosen ? `Set up ${serviceLabel(chosen.provider)}` : `Set up ${pinned ?? ""}`;

  return (
    <section className="px-4 lg:px-8 py-5 min-w-0" aria-labelledby="connections-add-title">
      <p className="text-label mb-3">
        <Link href={connectionsIndexHref()} className="lnk">
          ← Connections
        </Link>
      </p>
      <div className="conn-add-head conn-form">
        <h1 id="connections-add-title" className="text-section font-medium text-ink-100 break-words">
          {title}
        </h1>
        {viewer.operator && canWrite && chosen !== null && (
          <p className="text-micro text-ink-500 break-words">
            <Link href={connectionAddHref()} className="lnk">
              ← Choose a different service
            </Link>
          </p>
        )}
      </div>
      <p className="text-label text-ink-400 mt-1 break-words">
        Creating a connection grants no worker, app or run permissions.
      </p>

      {!viewer.operator ? (
        <PageState title="Operator access required">
          Connections are available to the operator. Use Sign in in the status bar if you
          have operator access.
        </PageState>
      ) : (
        <div className="conn-form mt-4">
          <StaleChip state={providersState ?? emptyGate} />
          <ResourceGate
            state={providersState ?? emptyGate}
            loading="loading services…"
            failed="could not load the registered services"
            onRetry={refresh}
          />
          {!canWrite ? (
            <PageState title="Read-only board">
              This board is read-only — connections cannot be added or changed here.
              <span className="block mt-3">
                <Link href={connectionsIndexHref()} className="lnk">
                  Back to connections
                </Link>
              </span>
            </PageState>
          ) : providersState?.data && page.kind === "add" ? (
            <ServiceChooser providers={addProviders} />
          ) : providersState?.data && page.kind === "addProvider" ? (
            chosen === null ? (
              <PageState title="Service unavailable">
                That service is not available for setup — it may have been removed or its
                reviewed enrollment metadata could not be read.
                <span className="block mt-3">
                  <Link href={connectionAddHref()} className="lnk">
                    Choose a different service
                  </Link>
                </span>
              </PageState>
            ) : (
              <>
                <AddConnection
                  // The draft (secret + consent) lives only in this
                  // component; keying on the provider means a route
                  // change to another service can never reuse it.
                  key={chosen.provider}
                  providers={addProviders}
                  existing={rows}
                  preset={chosen.provider}
                  hideProviderPicker
                  submitLabel="Save connection"
                  onClose={() => navigate(connectionsIndexHref())}
                  onAdded={(id) => {
                    refresh();
                    navigate(connectionDetailHref(id));
                  }}
                />
              </>
            )
          ) : null}
        </div>
      )}
    </section>
  );
}

/** The `/new` chooser: every registered service that offers a reviewed
 *  enrollment, with truthful reasons on the ones that do not. No fake
 *  services and no invented OAuth/API capabilities. */
function ServiceChooser({ providers }: { providers: ConnectionProvider[] }) {
  const enrollable = providers.filter((p) => enrollmentSupport(p).shapes.length > 0);
  const unsupported = providers.filter((p) => enrollmentSupport(p).shapes.length === 0);
  return (
    <div className="space-y-4">
      <p className="text-label text-ink-400 break-words">
        Choose a registered service to connect. Only the enrollment methods the service's
        reviewed descriptor declares are offered.
      </p>
      {enrollable.length === 0 ? (
        <p className="text-label text-ink-500 break-words">
          No registered service currently offers a reviewed enrollment.
        </p>
      ) : (
        <ul className="space-y-1.5">
          {enrollable.map((p) => (
            <li key={p.provider}>
              <Link href={connectionAddProviderHref(p.provider)} className="conn-row">
                <span className="conn-row-main">
                  <span className="text-label font-medium text-ink-100 break-words">
                    {serviceLabel(p.provider)}
                  </span>
                  {capabilityWords(p) && (
                    <span className="text-micro text-ink-500 break-words">
                      {capabilityWords(p)}
                    </span>
                  )}
                </span>
                <span className="conn-row-sub text-micro text-ink-500 break-words">
                  Connects with {enrollmentSupport(p).shapes.join(" or ")}
                </span>
              </Link>
            </li>
          ))}
        </ul>
      )}
      {unsupported.length > 0 && (
        <div>
          <h2 className="slabel mb-2">Not available to connect</h2>
          <ul className="space-y-1.5">
            {unsupported.map((p) => (
              <li key={p.provider} className="px-3 py-2 rounded border border-ink-700 bg-ink-850">
                <span className="text-label text-ink-300 break-words">{serviceLabel(p.provider)}</span>
                <span className="block text-micro text-ink-500 mt-0.5 break-words">
                  {enrollmentSupport(p).reason}
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}

/**
 * One connection's dedicated detail page (CAD-1319). The id comes from
 * the route; only a fresh successful list answer may report it missing
 * — a loading, failed or stale read never becomes a false not-found.
 */
function ConnectionsDetail({
  viewer,
  canWrite,
  id,
}: {
  viewer: { operator: boolean; readOnly: boolean };
  canWrite: boolean;
  id: string;
}) {
  const listRes = viewer.operator ? resources.connections : null;
  const providersRes = viewer.operator ? resources.connectionProviders : null;
  const listState = useMaybeResource(listRes);
  const providersState = useMaybeResource(providersRes);
  // CAD-1319 review: a row missing from a previously cached answer is
  // never a 404 — the entry forces one fetch for this mount (bypassing
  // revalidate's freshness skip) and only a completed successful answer
  // with no request in flight may call the id unknown. A failed or
  // stale fetch keeps its own state; it is never misread as missing.
  const [answered, setAnswered] = useState(false);
  useEffect(() => {
    setAnswered(false);
    if (!listRes) return;
    let live = true;
    void listRes.refresh().then(() => {
      if (live) setAnswered(true);
    });
    return () => {
      live = false;
    };
  }, [listRes, id]);
  useEffect(() => {
    if (providersRes) void providersRes.revalidate();
  }, [providersRes]);
  const rows = listState?.data ?? [];
  const providers = providersState?.data ?? [];
  const refresh = () => {
    if (listRes) void listRes.invalidate();
    if (providersRes) void providersRes.revalidate();
  };
  const row = rows.find((r) => r.id === id) ?? null;
  const provider = row ? providers.find((p) => p.provider === row.provider) ?? null : null;
  // The provider's descriptor is unknown while its resource is still
  // loading or failed — distinct from a loaded answer that genuinely
  // does not contain the provider. Metadata unknown is never shown as
  // "unsupported verification": verification reads unsupported only on
  // a loaded answer.
  const providersPending = providersState === null || providersState.status === "loading";
  const providersFailed = providersState?.status === "failed";
  const providerUnknown = row !== null && provider === null && (providersPending || providersFailed === true);
  // Settled-and-empty: this mount's fetch completed and the id is not
  // in the answer. An in-flight or stale/failed answer never qualifies.
  const notFound =
    row === null && answered && !listState?.inFlight &&
    (listState?.status === "ok" || listState?.status === "empty");

  return (
    <section className="px-4 lg:px-8 py-5 min-w-0" aria-labelledby="connection-detail-title">
      <div className="flex flex-wrap gap-2 items-center justify-between mb-4">
        <Link href={connectionsIndexHref()} className="lnk text-label">
          ← Connections
        </Link>
        {viewer.operator && (
          <div className="flex flex-wrap gap-2 items-center ms-auto">
            <StaleChip state={listState ?? emptyGate} />
            <StaleChip state={providersState ?? emptyGate} />
            <Button
              variant="ghost"
              icon={<IconRefresh />}
              loading={Boolean(listState?.inFlight || providersState?.inFlight)}
              onClick={refresh}
            >
              Refresh
            </Button>
          </div>
        )}
      </div>

      {!viewer.operator ? (
        <PageState title="Operator access required">
          Connections are available to the operator. Use Sign in in the status bar if you
          have operator access.
        </PageState>
      ) : (
        <>
          <ResourceGate
            state={listState ?? emptyGate}
            loading="loading connection…"
            failed="could not load the connection"
            onRetry={refresh}
          />
          {row !== null ? (
            <>
              {providerUnknown && (
                <p className="text-micro text-ink-500 mb-2 break-words" role="status">
                  {providersPending
                    ? "Loading this connection's service metadata…"
                    : "This connection's service metadata could not be loaded — capabilities and remote-verification status are unknown until it loads. The account below is intact."}
                  {providersFailed === true && (
                    <>
                      {" "}
                      <button className="lnk" onClick={() => providersRes && void providersRes.revalidate()}>
                        retry
                      </button>
                    </>
                  )}
                </p>
              )}
              <ConnectionDetail
                // Keyed to the exact account: a route change to another
                // id remounts the detail, so rotation/revoke drafts and
                // check/test generations can never carry across.
                key={row.id}
                row={row}
                provider={provider}
                capabilities={connectionCapabilities(providers, row.provider)}
                canWrite={canWrite}
                standalone
                providerMetaPending={providerUnknown}
                onChanged={refresh}
                onRevoked={() => {
                  refresh();
                  navigate(connectionsIndexHref());
                }}
              />
            </>
          ) : notFound ? (
            <PageState title="Connection not found">
              This connection is not on this board — it may have been disconnected or the
              link is out of date.
              <span className="block mt-3">
                <Link href={connectionsIndexHref()} className="lnk">
                  Back to connections
                </Link>
              </span>
            </PageState>
          ) : listState?.data || listState?.status === "stale" || listState?.status === "failed" ? (
            // The list answered once but is refreshing or failed over:
            // the missing row may simply be stale — never a 404.
            <div className="card px-4 py-5 text-secondary text-ink-400" role="status">
              Checking this connection against the latest account list…
            </div>
          ) : null}
        </>
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
  standalone = false,
  providerMetaPending = false,
  onChanged,
  onRevoked,
}: {
  row: Connection;
  provider?: ConnectionProvider | null;
  capabilities: string | null;
  canWrite: boolean;
  /** Standalone page: the card owns the heading; skip the inner title. */
  standalone?: boolean;
  /** Provider metadata is still loading or failed — its absence is not
   *  evidence the provider is gone, so verification/support copy reads
   *  unknown rather than unsupported. */
  providerMetaPending?: boolean;
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
  const verify = providerMetaPending
    ? { supported: false, reason: "" }
    : verificationSupport(provider ?? null);
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
      {standalone ? (
        <h1 id="connection-detail-title" className="text-section font-medium text-ink-100 break-words">
          {connectionLabel(row)}
        </h1>
      ) : (
        <h2 className="text-cardtitle font-medium text-ink-100 break-words">
          {connectionLabel(row)}
        </h2>
      )}
      <dl className="mt-2 space-y-1 text-label min-w-0">
        <div className="flex flex-wrap gap-x-2">
          <dt className="text-ink-500">Account</dt>
          <dd className="text-ink-200 break-words">
            {row.provider} · {row.account}
          </dd>
        </div>
        <div className="flex flex-wrap gap-x-2 min-w-0">
          <dt className="text-ink-500">Readiness</dt>
          <dd className="text-ink-200 break-words">{readinessText(row)}</dd>
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
              {providerMetaPending
                ? "unknown — the service's metadata is still loading"
                : verify.supported
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
              {testing ? "Testing…" : "Test SMTP login"}
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
  // CAD-1319 review: a revoke is bound to the exact mounted row context —
  // id + credential revision + registration digest. A row swap (route or
  // refresh), a close, a rotation or an unmount invalidates the request,
  // so a late success can never navigate away from a newer view and a
  // late failure never reports on a different account.
  const revokeGen = useRef(0);
  const ctx = `${row.id}:${row.revision ?? "x"}:${row.registration_digest ?? "x"}`;
  const ctxRef = useRef(ctx);
  ctxRef.current = ctx;
  // Commit-phase cleanup (see AddConnection): an unmount driven by a
  // synchronous store change invalidates the in-flight revoke before the
  // next view commits, so a late success never navigates away.
  useLayoutEffect(() => {
    return () => {
      revokeGen.current += 1; // unmount/close invalidates any in-flight revoke
    };
  }, []);

  const submit = () => {
    if (busy || !armed) return;
    setBusy(true);
    setError(null);
    const gen = ++revokeGen.current;
    const want = ctxRef.current;
    api
      .connectionRevoke(row.id)
      .then(() => {
        if (gen !== revokeGen.current || ctxRef.current !== want) return;
        onDone();
      })
      .catch((e: ApiError) => {
        if (gen !== revokeGen.current || ctxRef.current !== want) return;
        setError(e.message ?? String(e));
      })
      .finally(() => {
        if (gen === revokeGen.current) setBusy(false);
      });
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

/** Enroll a scoped token or SMTP sender for a supported provider. The
 *  credential clears the moment the request settles. When `preset`
 *  pins the provider (the dedicated `/new/<provider>` page) the
 *  redundant Service dropdown is omitted (`hideProviderPicker`) and the
 *  heading comes from the page, not this card. */
export function AddConnection({
  providers,
  existing,
  preset,
  hideProviderPicker = false,
  submitLabel,
  onClose,
  onAdded,
}: {
  providers: ConnectionProvider[];
  existing: Connection[];
  preset?: string | null;
  /** Pinned presentation: omit the Service dropdown — the page names the service. */
  hideProviderPicker?: boolean;
  /** Primary action label — "Add connection" on the generic card. */
  submitLabel?: string;
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
  const customSmtp = smtpShape && smtpPreset.id === "other";

  // CAD-1319 review: every in-flight enroll request is bound to a draft
  // generation. Any draft-context change — reset, provider, method,
  // preset, descriptor fingerprint or unmount — bumps it, so a late
  // completion can never navigate, set an error/risk state on a newer
  // draft, clear a newer secret or release a newer request's `busy`.
  // An invalidated success is still a real enrollment on the daemon —
  // the response is dropped silently here, never claimed "not stored".
  const requestGen = useRef(0);
  const live = (gen: number) => gen === requestGen.current;
  // Commit-phase cleanup: an unmount driven by a synchronous store
  // change (or an operator-loss meta swap) invalidates in-flight enroll
  // before the blocked view commits — a passive cleanup would leave a
  // window where the old completion still reads as live.
  useLayoutEffect(() => {
    return () => {
      requestGen.current += 1; // unmount invalidates any in-flight enroll
    };
  }, []);

  const resetDraft = () => {
    requestGen.current += 1; // an in-flight request's settle can no longer act
    setToken("");
    setScopePicks(new Set());
    setAccount("");
    setAcceptRisk(false);
    setRiskNeeded(false);
    setError(null);
    setBusy(false); // release the superseded request's busy — it stays ignored
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
    // A preset change retargets the SMTP service: a different mail
    // server is a different draft context — an in-flight request's
    // completion, the password and the custody consent never carry into
    // it. The non-secret account/sender fields may stay; transport
    // follows the new preset's paired host/port/TLS.
    requestGen.current += 1;
    setSmtpPreset(next);
    setToken("");
    setAcceptRisk(false);
    setRiskNeeded(false);
    setBusy(false); // release the superseded request's busy — it stays ignored
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
        setError("Fill in the host, port, login, password and sender address for the SMTP sender.");
        return;
      }
      const portTlsError = smtpPortTlsError(smtp.host.trim(), port, smtp.tls_mode);
      if (portTlsError) {
        setError(portTlsError);
        return;
      }
      setBusy(true);
      setError(null);
      const gen = requestGen.current;
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
        .then((out) => {
          // Only a completion on the exact draft context that issued the
          // request may navigate. An obsolete/unmounted draft drops the
          // result — it may still be a real enrollment; it is simply not
          // this draft's to claim.
          if (!live(gen)) return;
          onAdded(out.connection.id);
        })
        // CAD-1013: typed-code safe reason — never the raw downstream
        // message, which can echo the SMTP host/password just typed.
        // custody_unprotected surfaces the risk toggle near Save.
        .catch((e: ApiError) => {
          if (!live(gen)) return; // never clobber a newer draft's state
          setRiskNeeded(e instanceof ApiError && e.code === "custody_unprotected");
          setError(enrollErrorMessage(e));
        })
        .finally(() => {
          if (!live(gen)) return;
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
    const gen = requestGen.current;
    api
      .connectionCreate({
        provider,
        account: cleanedAccount,
        shape: "token",
        token: token.trim(),
        scopes: wanted,
        ...(acceptRisk ? { accept_same_uid_risk: true } : {}),
      })
      .then((out) => {
        if (!live(gen)) return; // obsolete/unmounted draft — never navigate
        onAdded(out.connection.id);
      })
      // CAD-1013: typed-code safe reason — never raw downstream text.
      .catch((e: ApiError) => {
        if (!live(gen)) return;
        setRiskNeeded(e instanceof ApiError && e.code === "custody_unprotected");
        setError(enrollErrorMessage(e));
      })
      .finally(() => {
        if (!live(gen)) return;
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
  const primaryLabel = submitLabel ?? "Add connection";

  return (
    <section className={hideProviderPicker ? "min-w-0" : "card px-4 py-3.5 min-w-0"} aria-label="add connection">
      {!hideProviderPicker && (
        <h2 className="text-cardtitle font-medium text-ink-100">
          {chosen ? `Set up ${chosen.provider}` : "Add connection"}
        </h2>
      )}
      {candidates.length === 0 ? (
        <p className="text-label text-ink-400 mt-2 break-words">
          No provider currently offers enrollment. {existing.length > 0 && (
            <>Existing connections are listed below.</>
          )}
        </p>
      ) : (
        <form
          className="mt-2 space-y-4"
          onSubmit={(e) => {
            e.preventDefault();
            submit();
          }}
        >
          {!hideProviderPicker && (
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
                    {serviceLabel(p.provider)}
                    {capabilityWords(p) ? ` — ${capabilityWords(p)}` : ""}
                  </option>
                ))}
              </select>
              {provider === "" && (
                <p className="text-micro text-ink-500 mt-1 break-words">
                  Choose a service to connect.
                </p>
              )}
            </div>
          )}
          {unavailable && (
            <p className="text-micro text-warn mt-1 break-words" role="alert">
              That service is no longer available — pick another service to connect, or
              cancel. Nothing typed here can go to a different service.
            </p>
          )}
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
              Choose how to connect {serviceLabel(provider)} — a scoped API token, or an
              SMTP sender.
            </p>
          )}
          {!methodPending && (
          <>
          {smtpShape && (
          <fieldset className="space-y-2">
            <legend className="text-label font-medium text-ink-200">Email provider</legend>
            <p className="text-micro text-ink-500 break-words">
              SMTP server configuration — encrypted submission only.
            </p>
            <div role="group" aria-label="Email provider" className="flex flex-wrap gap-2">
              {PRESETS.map((p) => (
                <Button
                  key={p.id}
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
          )}
          <fieldset className="space-y-2">
            <legend className="text-label font-medium text-ink-200">Account</legend>
            <div>
              <label htmlFor={accountId} className="text-label text-ink-300">
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
          </fieldset>
          <fieldset className="space-y-2">
            <legend className="text-label font-medium text-ink-200">Credentials</legend>
            {!smtpShape && (
              <div>
                <p className="text-label text-ink-300">
                  Permissions{" "}
                  <span className="text-ink-500 font-normal">(declare only what the provider granted)</span>
                </p>
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
              </div>
            )}
            {smtpShape ? (
              <>
                <div>
                  <label htmlFor={`${tokenId}-username`} className="text-label text-ink-300">
                    {customSmtp ? "SMTP username" : "Your email address"}
                  </label>
                  <input
                    id={`${tokenId}-username`}
                    type="text"
                    autoComplete="off"
                    spellCheck={false}
                    value={smtp.username}
                    onChange={(e) => setSmtp((cur) => ({ ...cur, username: e.target.value }))}
                    placeholder={customSmtp ? "smtp username" : "you@yourcompany.com"}
                    className="field w-full mt-1"
                    disabled={busy}
                  />
                </div>
                <div>
                  <label htmlFor={tokenId} className="text-label text-ink-300">
                    {customSmtp ? "SMTP password" : "App password"}
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
              </>
            ) : (
              <div>
                <label htmlFor={tokenId} className="text-label text-ink-300">
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
          </fieldset>
          {smtpShape && (
          <fieldset className="space-y-2">
            <legend className="text-label font-medium text-ink-200">Sender</legend>
            <div>
              <label htmlFor={`${tokenId}-sender`} className="text-label text-ink-300">
                Sender address
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
            <div
              className="conn-risk text-ink-200 border border-warn/40 rounded px-3 py-2 bg-warn/10"
              data-risk-needed
            >
              <div className="conn-risk-title">
                <IconWarning size={14} />
                <strong>Storage consent required</strong>
              </div>
              <p className="conn-risk-desc">
                This server cannot isolate saved SMTP passwords from agents using the same
                system account.
              </p>
              <label className="flex items-start gap-2">
                <input
                  type="checkbox"
                  checked={acceptRisk}
                  onChange={(e) => setAcceptRisk(e.target.checked)}
                  disabled={busy}
                  className="mt-0.5"
                />
                <span>I accept storing this password here.</span>
              </label>
              <p className="conn-risk-hint">This is never pre-selected.</p>
            </div>
          )}
          <div className="conn-actions flex flex-wrap gap-2">
            <Button variant="primary" type="submit" loading={busy} disabled={cannotSubmit}>
              {busy ? "Saving…" : primaryLabel}
            </Button>
            <Button variant="ghost" onClick={onClose} disabled={busy}>
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
