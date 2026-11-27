import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { cache, resources } from "../../lib/resources";
import { useMaybeResource } from "../../lib/useResource";
import { EMAIL_CONNECTIONS_HREF, EMAIL_SENDING_HREF, routePath } from "../../lib/router";
import { navigate } from "../../lib/useLocation";
import type { Connection } from "../../lib/types";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import Select from "../../ui/Select";
import PageState from "../../ui/PageState";
import { IconApps, IconArrowDown, IconOutbox, IconSettings } from "../../ui/icons";
import type { Viewer } from "../projects/work";
import ConfirmDialog from "../app-shell/shared/ConfirmDialog";
import SettingsDrawer from "./SettingsDrawer";
import EmailSetup from "./EmailSetup";
import { RotateForm, RevokeConfirm } from "./Connections";
import { useProtectedEntry } from "../connections/useProtectedEntry";
import { errorCopy, openTrustedEntry } from "../connections/credentialRequestView";
import { newAudienceId } from "../app-shell/segmentGrammar";
import {
  friendlySendError,
  parseOriginReceipt,
  parseSmtpBinding,
  readSmtpBinding,
  sendClient,
  type SmtpBinding,
} from "../app-shell/sendClient";
import type { HostScope } from "../app-shell/hostActions";
import { connectionNeedsAttention, isSmtpSender } from "./connectionsView";
import { HOSTED_ACCOUNT } from "./hostedSmtpView";
import { workspaceApps, type Installation } from "../workspace-apps/workspaceApps";
import {
  isHostedSender,
  isHostedTransport,
  senderChoices,
  senderLine,
  sendingFrom,
} from "./emailSendingView";
import { useTranslate } from "../../lib/locale";

/** Connections → Email: enroll sending accounts and choose a sender per app context. */

interface Target {
  scope: HostScope;
  label: string;
}

/**
 * A disabled or soft-removed install keeps `smtp_custody` but its contexts
 * endpoint answers 404. Legacy/local responses omit the lifecycle fields and
 * mean active.
 */
function activeInstall(install: Installation): boolean {
  if (install.state === "disabled" || install.state === "removed") return false;
  if (install.disabled_at !== undefined && install.disabled_at !== null) return false;
  if (install.removed_at !== undefined && install.removed_at !== null) return false;
  if (install.removed !== undefined && install.removed !== null) return false;
  return true;
}

/** Every active context of an install that declares SMTP sending, flattened. */
async function loadTargets(): Promise<Target[]> {
  const installs = (await workspaceApps.installations()).filter(
    (i) => i.smtp_custody === true && activeInstall(i),
  );
  const out: Target[] = [];
  for (const install of installs) {
    const contexts = await workspaceApps.contexts(install.install_id);
    for (const ctx of contexts.filter((c) => c.state === "active")) {
      out.push({
        scope: { installId: install.install_id, contextId: ctx.id },
        label: appContextLabel(install.title || install.name, ctx.config.label || ctx.id),
      });
    }
  }
  return out;
}

const targetsResource = cache.resource<Target[]>("email-targets", loadTargets, { freshMs: 5_000 });
const bindingByKey = cache.family("email-binding", (key: string) => {
  const [installId, contextId] = JSON.parse(key) as [string, string];
  return readSmtpBinding({ installId, contextId });
}, { freshMs: 5_000 });

function bindingResource(scope: HostScope) {
  return bindingByKey(JSON.stringify([scope.installId, scope.contextId]));
}

function invalidateAccounts() {
  resources.connections.markInvalid();
  cache.invalidate("email-binding");
  cache.invalidate("smtp-connections");
}

// Stored status note: a UI-owned key (plus data), translated at render.
type SendNote = { key: "chosen" | "changed" | "removed" | "origin-cleared" | "origin-set"; value?: string };
function renderNote(note: SendNote | null, t: (english: string) => string): string | null {
  if (!note) return null;
  switch (note.key) {
    case "chosen": return t("Sender chosen.");
    case "changed": return t("Sender changed.");
    case "removed": return t("Sender removed.");
    case "origin-cleared": return t("Your own address was cleared.");
    case "origin-set": return t("Unsubscribe links now use {origin}.").replace("{origin}", () => note.value ?? "");
  }
}

/** "CRM · CRM" says nothing twice; the second part earns its place only by differing. */
function appContextLabel(app: string, context: string): string {
  return app === context ? app : `${app} · ${context}`;
}

export default function EmailSending({
  viewer,
  fixedUnsubscribeAddress = false,
  focusSenders = false,
}: {
  viewer: Viewer;
  /** A hosted board's links always use its own address, so there's nothing to set. */
  fixedUnsubscribeAddress?: boolean;
  focusSenders?: boolean;
}) {
  const t = useTranslate();
  const targetState = useMaybeResource(viewer.operator === true ? targetsResource : null);
  const targets = targetState?.data ?? null;
  const accountResource = viewer.operator === true ? resources.smtpConnections(fixedUnsubscribeAddress ? "hosted" : "native") : null;
  const accountState = useMaybeResource(accountResource);
  const rows = accountState?.data?.connections?.filter(isSmtpSender) ?? null;
  const hostedSmtp = accountState?.data?.hosted_smtp === true;
  const partial = accountState?.data?.connections_unavailable !== undefined;
  const accountsError = accountState?.status === "failed" || accountState?.status === "stale" ? accountState.error : null;
  const [accountsAnswered, setAccountsAnswered] = useState(false);
  const accountCurrent = accountsAnswered && accountState !== null && !accountState.inFlight && accountState.status === "ok" && !partial;
  const targetsError = targetState?.status === "failed" || targetState?.status === "stale" ? targetState.error : null;
  // One drawer at a time: rows stay reachable by keyboard behind an open one.
  const [openRow, setOpenRow] = useState<string | null>(null);
  const sendersHeading = useRef<HTMLHeadingElement>(null);
  const accountsHeading = useRef<HTMLHeadingElement>(null);
  const accountAdded = useRef(false);
  const sendersFocused = useRef(false);
  useEffect(() => {
    if (!focusSenders || viewer.operator !== true) sendersFocused.current = false;
    if (viewer.operator !== true) return;
    if (accountAdded.current && accountCurrent) {
      accountAdded.current = false;
      accountsHeading.current?.focus();
    } else if (!accountAdded.current && focusSenders && !sendersFocused.current) {
      sendersHeading.current?.focus();
      sendersFocused.current = true;
    }
  }, [focusSenders, accountCurrent, viewer.operator]);

  const canAdd = viewer.operator === true && !viewer.readOnly && accountCurrent && rows !== null
    && !(hostedSmtp && rows.some((row) => row.provider === "smtp" && row.account === HOSTED_ACCOUNT));

  useEffect(() => {
    setAccountsAnswered(false);
    if (!accountResource) return;
    let live = true;
    void accountResource.revalidate().then(() => { if (live) setAccountsAnswered(true); });
    return () => { live = false; };
  }, [accountResource]);

  useEffect(() => {
    if (viewer.operator === true) void targetsResource.revalidate();
  }, [viewer.operator]);

  return (
    <section className="px-4 lg:px-8 py-5 min-w-0" aria-labelledby="email-sending-title">
      <Link href="/settings/connections" className="lnk text-label">{t("← Connections")}</Link>
      <header className="flex items-center gap-3 mt-3 pb-4 mb-4 border-b border-ink-700">
        <span className="inline-flex items-center justify-center rounded-lg bg-accent/10 text-accent size-10 shrink-0" aria-hidden="true"><IconOutbox size={21} /></span>
        <div className="min-w-0">
          <h1 id="email-sending-title" className="text-section font-medium text-ink-100">{t("Email")}</h1>
          <p className="text-label text-ink-400 mt-1 break-words">{t("Manage sending accounts and choose which one each app uses.")}</p>
        </div>
      </header>
      {viewer.operator === null ? (
        <PageState title={viewer.access === "unavailable" ? t("Access could not be confirmed") : t("Checking access…")}>
          {viewer.access === "unavailable" ? (
            <>
              {t("The board's access check did not answer, so email sending stays closed.")}{" "}
              {viewer.onRetryAccess && (
                <Button size="sm" onClick={viewer.onRetryAccess}>{t("Retry access check")}</Button>
              )}
            </>
          ) : (
            t("Checking whether this session may manage email sending…")
          )}
        </PageState>
      ) : !viewer.operator ? (
        <PageState title={t("Operator access required")}>{t("Email sending is available to the operator. Use Sign in in the status bar if you have operator access.")}</PageState>
      ) : (
        <div className="grid gap-6 min-w-0">
          <EmailAccounts headingRef={accountsHeading} rows={rows ?? []} complete={accountCurrent} canAdd={canAdd} hostedSmtp={hostedSmtp} onAdd={() => setOpenRow("add")} />
          {accountState?.discarded ? (
            <p role="alert" className="text-label text-fail flex flex-wrap items-center gap-2">
              {t("Account access could not be confirmed.")}{" "}
              <Button size="sm" onClick={() => { if (accountResource) void accountResource.refresh(); }}>{t("Retry access check")}</Button>
            </p>
          ) : accountsError !== null ? (
            <p className="text-label text-fail flex flex-wrap items-center gap-2" role="alert">
              {accountsError}
              <Button size="sm" onClick={() => { if (accountResource) void accountResource.refresh(); }}>{t("Retry")}</Button>
            </p>
          ) : rows === null ? (
            <p className="text-label text-ink-400" role="status">{t("Loading…")}</p>
          ) : partial ? (
            <p className="text-label text-fail flex flex-wrap items-center gap-2" role="alert" data-state="connections-partial">
              {t("Some connected accounts couldn't be loaded, so this list may be incomplete.")}
              <Button size="sm" onClick={() => { if (accountResource) void accountResource.refresh(); }}>{t("Retry")}</Button>
            </p>
          ) : null}
          <section aria-labelledby="app-senders">
            <h2 ref={sendersHeading} id="app-senders" tabIndex={-1} className="text-cardtitle font-medium text-ink-100">{t("App senders")}</h2>
            <p className="text-label text-ink-400 mt-1 mb-2">{t("Choose the account each app sends from.")}</p>
            {targetsError !== null ? (
              <p className="text-label text-fail flex flex-wrap items-center gap-2" role="alert">
                {targetsError}
                <Button size="sm" onClick={() => { void targetsResource.refresh(); }}>{t("Retry")}</Button>
              </p>
            ) : targets === null ? (
              <p className="text-label text-ink-400" role="status">{t("Loading…")}</p>
            ) : targets.length === 0 && (
              <p className="text-label text-ink-400 mb-2" data-state="no-apps">{t("No installed app sends email, so there is nothing to choose yet.")}</p>
            )}
            {((targets?.length ?? 0) > 0 || !fixedUnsubscribeAddress) && (
              <div className="settings-rows" aria-label={t("Apps that send email")}>
                {(targets ?? []).map((target) => (
                  <SenderRow
                    key={`${target.scope.installId}:${target.scope.contextId}`}
                    open={openRow === `${target.scope.installId}:${target.scope.contextId}`}
                    onOpen={() => setOpenRow(`${target.scope.installId}:${target.scope.contextId}`)}
                    onClose={() => setOpenRow(null)}
                    target={target}
                    rows={accountCurrent ? rows : null}
                    hostedSmtp={hostedSmtp}
                    viewer={{ ...viewer, readOnly: viewer.readOnly || targetState?.status !== "ok" || targetState.inFlight }}
                  />
                ))}
                {!fixedUnsubscribeAddress && (
                  <OriginRow
                    viewer={viewer}
                    open={openRow === "origin"}
                    onOpen={() => setOpenRow("origin")}
                    onClose={() => setOpenRow(null)}
                  />
                )}
              </div>
            )}
          </section>
          {canAdd && rows !== null && openRow === "add" && (
            <EmailSetup
              rows={rows}
              hostedSmtp={hostedSmtp}
              onClose={() => setOpenRow(null)}
              onSaved={() => {
                accountAdded.current = true;
                setOpenRow(null);
                invalidateAccounts();
              }}
            />
          )}
        </div>
      )}
    </section>
  );
}

/** SMTP account management never needs the provider directory or broad connection list. */
export function EmailAccount({ id, viewer, hostedBoard }: { id: string; viewer: Viewer; hostedBoard: boolean }) {
  const t = useTranslate();
  const resource = viewer.operator === true ? resources.smtpConnections(hostedBoard ? "hosted" : "native") : null;
  const state = useMaybeResource(resource);
  const [answered, setAnswered] = useState(false);
  useEffect(() => {
    setAnswered(false);
    if (!resource) return;
    let live = true;
    // A cached hit can render at once. A miss from a prior list is not a 404.
    const cached = resource.get().data?.connections?.some((row) => row.id === id && isSmtpSender(row));
    void (cached ? resource.revalidate() : resource.refresh()).then(() => { if (live) setAnswered(true); });
    return () => { live = false; };
  }, [resource, id]);
  const row = state?.data?.connections?.find((item) => item.id === id && isSmtpSender(item)) ?? null;
  const unavailable = state?.status === "failed" || state?.status === "stale" || state?.data?.connections_unavailable !== undefined;
  const current = answered && state?.status === "ok" && !state.inFlight && !unavailable;
  const confirmCurrentAccount = async (): Promise<Connection | null> => {
    if (!resource || viewer.readOnly) return null;
    await resource.refresh();
    const snapshot = resource.get();
    if (snapshot.inFlight || snapshot.status !== "ok" || snapshot.data?.connections_unavailable !== undefined) return null;
    return snapshot.data?.connections.find((item) => item.id === id && item.kind === "enrolled" && isSmtpSender(item)) ?? null;
  };
  return (
    <section className="px-4 lg:px-8 py-5 min-w-0">
      <Link href={EMAIL_CONNECTIONS_HREF} className="lnk text-label">{t("← Email accounts")}</Link>
      {viewer.operator !== true ? (
        <PageState title={viewer.access === "unavailable" ? t("Access could not be confirmed") : viewer.operator === null ? t("Checking access…") : t("Operator access required")}>
          {viewer.access === "unavailable" && viewer.onRetryAccess && <Button size="sm" onClick={viewer.onRetryAccess}>{t("Retry access check")}</Button>}
        </PageState>
      ) : state?.discarded ? (
        <PageState title={t("Account access could not be confirmed.")}>
          <Button size="sm" onClick={() => { if (resource) void resource.refresh(); }}>{t("Retry access check")}</Button>
        </PageState>
      ) : (
        <>
          {unavailable && (
            <p role="alert" className="text-label text-fail mt-3">{t("Account information could not be fully confirmed.")}{" "}
              <Button size="sm" onClick={() => { if (resource) void resource.refresh(); }}>{t("Retry")}</Button>
            </p>
          )}
          {row ? <EmailAccountOverview row={row} current={current} canWrite={current && !viewer.readOnly} hostedBoard={hostedBoard} confirmCurrentAccount={confirmCurrentAccount} /> :
            current ? <PageState title={t("Email account not found")}>{t("This account may have been disconnected.")}</PageState> :
              !unavailable && <p role="status" className="text-label text-ink-400 mt-4">{t("Loading account…")}</p>}
        </>
      )}
    </section>
  );
}

function EmailAccountOverview({ row, current, canWrite, hostedBoard, confirmCurrentAccount }: {
  row: Connection;
  current: boolean;
  canWrite: boolean;
  hostedBoard: boolean;
  confirmCurrentAccount: () => Promise<Connection | null>;
}) {
  const t = useTranslate();
  const entry = useProtectedEntry(!hostedBoard);
  const [rotating, setRotating] = useState(false);
  const [disconnecting, setDisconnecting] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [usedBy, setUsedBy] = useState<string[] | null>(null);
  const [usageUnknown, setUsageUnknown] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const onScreen = useRef(false);
  useLayoutEffect(() => {
    onScreen.current = true;
    return () => { onScreen.current = false; };
  }, []);
  useEffect(() => {
    let live = true;
    setUsedBy(null);
    setUsageUnknown(false);
    void (async () => {
      await (attempt ? targetsResource.refresh() : targetsResource.revalidate());
      const targets = targetsResource.get();
      if (targets.status !== "ok" || !targets.data) {
        if (live) { setUsedBy([]); setUsageUnknown(true); }
        return;
      }
      const reads = targets.data.map(async (target) => {
        const resource = bindingResource(target.scope);
        await (attempt ? resource.refresh() : resource.revalidate());
        const state = resource.get();
        return { target, state };
      });
      const results = await Promise.all(reads);
      if (!live) return;
      setUsedBy(results.filter(({ state }) => state.status === "ok" && state.data?.state === "live" && state.data.connectionId === row.id).map(({ target }) => target.label));
      setUsageUnknown(results.some(({ state }) => state.status !== "ok" || state.inFlight));
    })();
    return () => { live = false; };
  }, [row.id, attempt]);
  const ready = row.smtp != null && row.status.custody_available && !connectionNeedsAttention(row);
  const label = row.smtp?.sender_name || row.account;
  const update = async () => {
    if (!canWrite || busy || (!hostedBoard && entry.status !== "ready")) return;
    setBusy(true);
    setError(null);
    try {
      // The overview may use a warm snapshot. Rotation inherits omitted
      // settings, so confirm the exact account before offering the form.
      const account = await confirmCurrentAccount();
      if (!account || !onScreen.current) return;
      if (hostedBoard || (entry.status === "ready" && !entry.entry.configured)) {
        setRotating(true);
        return;
      }
      const { request } = await api.credentialRequestCreate({
        provider: account.provider, verb: "rotate", connection_id: account.id,
        ...(account.revision !== null ? { expected_credential_revision: account.revision } : {}),
      });
      if (onScreen.current && !openTrustedEntry(request)) setError(errorCopy("unsupported_deployment"));
    } catch (error) {
      if (onScreen.current) setError(errorCopy(error instanceof ApiError ? error.code : null));
    } finally {
      if (onScreen.current) setBusy(false);
    }
  };
  return (
    <div className="grid gap-6 mt-4">
      <header className="flex flex-wrap items-center justify-between gap-4 border-b border-ink-700 pb-5">
        <div className="flex items-center gap-3 min-w-0">
          <span className="inline-flex items-center justify-center rounded-lg bg-accent/10 text-accent size-10 shrink-0" aria-hidden="true"><IconOutbox size={21} /></span>
          <div className="min-w-0">
            <div className="flex flex-wrap items-center gap-2">
              <h1 className="text-section font-medium text-ink-100 break-words">{label}</h1>
              <span className={`chip ${current && ready ? "bg-ok/10 text-ok" : "bg-warn/10 text-warn"}`}>
                {!current ? t("Not confirmed") : ready ? t("Ready") : t("Needs attention")}
              </span>
            </div>
            {row.smtp?.sender && <p className="text-label text-ink-400 break-words">{row.smtp.sender}</p>}
          </div>
        </div>
        {row.kind === "enrolled" && (
          <Button variant="primary" className="max-sm:w-full" disabled={!canWrite || (!hostedBoard && entry.status !== "ready")} loading={busy} onClick={update}>
            {t("Update credentials")}
          </Button>
        )}
      </header>
      {error && <p role="alert" className="text-label text-fail">{error}</p>}
      {!hostedBoard && entry.status === "failed" && <p role="alert" className="text-label text-fail">{t("Secure credential settings could not be loaded.")}</p>}
      {canWrite && rotating && (
        <SettingsDrawer title={t("Update credentials")} onClose={() => setRotating(false)}>
          <RotateForm row={row} hostedBoard={hostedBoard} onClose={() => setRotating(false)} onDone={() => { setRotating(false); invalidateAccounts(); }} />
        </SettingsDrawer>
      )}
      <section aria-labelledby="email-used-by">
        <h2 id="email-used-by" className="text-cardtitle font-medium text-ink-100 mb-2">{t("Used by apps")}</h2>
        {usedBy === null && <p role="status" className="text-label text-ink-400">{t("Loading app senders…")}</p>}
        {!!usedBy?.length && (
          <div className="settings-rows">
            <div className="settings-row flex-wrap">
              <span className="inline-flex items-center justify-center rounded bg-accent/10 text-accent size-8 shrink-0" aria-hidden="true">
                <IconApps />
              </span>
              <span className="settings-row-text"><b>{usedBy.join(", ")}</b></span>
              <Link href={EMAIL_SENDING_HREF} className="lnk">{t("View app senders")}</Link>
            </div>
          </div>
        )}
        {usedBy?.length === 0 && !usageUnknown && (
          <div className="flex items-center justify-between gap-3 flex-wrap">
            <p className="text-label text-ink-400">{t("No app currently uses this account.")}</p>
            <Link href={EMAIL_SENDING_HREF} className="lnk">{t("View app senders")}</Link>
          </div>
        )}
        {usageUnknown && (
          <p role="alert" className="text-label text-fail">{t("Some app sender bindings could not be loaded; usage may be incomplete.")}{" "}
            <Button size="sm" onClick={() => setAttempt((n) => n + 1)}>{t("Retry")}</Button>
          </p>
        )}
      </section>
      <details className="group rounded-lg border border-ink-700 text-label">
        <summary className="flex cursor-pointer list-none items-center gap-3 px-3 py-2.5">
          <IconSettings className="shrink-0 text-ink-400" aria-hidden="true" />
          <span className="grow min-w-0">
            <strong className="font-medium text-ink-100">{t("Advanced settings")}</strong>
            <span className="block text-ink-400">{t("SMTP configuration and connection ID")}</span>
          </span>
          <IconArrowDown className="shrink-0 text-ink-400 group-open:rotate-180" aria-hidden="true" />
        </summary>
        <div className="border-t border-ink-700 px-3 py-2.5">
          <dl className="grid gap-2 break-words">
            <div><dt className="text-ink-400">{t("SMTP server")}</dt><dd>{row.smtp?.host ?? t("Unavailable")}</dd></div>
            <div><dt className="text-ink-400">{t("Port and encryption")}</dt><dd>{row.smtp ? `${row.smtp.port} · ${row.smtp.tls_mode === "implicit" ? t("Implicit TLS") : row.smtp.tls_mode === "starttls" ? "STARTTLS" : row.smtp.tls_mode}` : t("Unavailable")}</dd></div>
            <div><dt className="text-ink-400">{t("SMTP username")}</dt><dd>{row.smtp?.username ?? t("Unavailable")}</dd></div>
            <div><dt className="text-ink-400">{t("Connection ID")}</dt><dd><code>{row.id}</code></dd></div>
          </dl>
          {!hostedBoard && (
            <Link className="lnk mt-3 inline-block" href={routePath({ screen: "settings", section: "connections", page: { kind: "detail", id: row.id } })}>
              {t("Connection diagnostics")}
            </Link>
          )}
        </div>
      </details>
      {row.kind === "enrolled" && (
        <footer className="border-t border-ink-700 pt-2">
          <Button variant="ghost" className="text-fail!" disabled={!canWrite} onClick={() => setDisconnecting(true)}>
            {t("Disconnect account")}
          </Button>
          {canWrite && disconnecting && (
            <SettingsDrawer title={t("Disconnect account")} onClose={() => setDisconnecting(false)}>
              <div className="grid gap-3">
                {hostedBoard && !!usedBy?.length ? (
                  <p className="text-label text-ink-300 break-words">
                    {t("Stop using this account in {apps} before disconnecting.").replace("{apps}", () => usedBy.join(", "))}
                  </p>
                ) : (
                  <>
                    {!hostedBoard && !!usedBy?.length && <p className="text-label text-ink-300 break-words">{t("{apps} will stop sending from this address.").replace("{apps}", () => usedBy.join(", "))}</p>}
                    {(usedBy === null || usageUnknown) && <p className="text-label text-ink-300">
                      {hostedBoard
                        ? t("App usage could not be fully confirmed. Disconnect is refused while an app still uses this account.")
                        : t("App usage could not be fully confirmed. Any app using this account will stop sending from it.")}
                    </p>}
                    <RevokeConfirm row={row} hostedSmtp={hostedBoard} onClose={() => setDisconnecting(false)} onDone={() => { invalidateAccounts(); navigate(EMAIL_CONNECTIONS_HREF); }} />
                  </>
                )}
                {hostedBoard && <Link className="lnk" href={EMAIL_SENDING_HREF} onClick={() => setDisconnecting(false)}>{t("View app senders")}</Link>}
              </div>
            </SettingsDrawer>
          )}
        </footer>
      )}
    </div>
  );
}

/** One app: which account it sends from. The choice lives in the drawer. */
function SenderRow({ target, rows, hostedSmtp, viewer, open, onOpen, onClose }: {
  open: boolean;
  onOpen: () => void;
  onClose: () => void;
  target: Target;
  rows: Connection[] | null;
  hostedSmtp: boolean;
  viewer: Viewer;
}) {
  const t = useTranslate();
  const resource = bindingResource(target.scope);
  const state = useMaybeResource(resource);
  const binding = state?.data;
  const failed = state?.status === "failed" || state?.status === "stale";
  useEffect(() => {
    void resource.revalidate();
  }, [resource]);
  const live = sendingFrom(binding);
  const line = failed
    ? t("Couldn’t load the sender")
    : state === null || state.status === "loading"
      ? t("Reading…")
      : binding === null
        ? t("No sender chosen")
        : live ?? t("The chosen sender needs re-checking");
  return (
    <>
      <div className="flex items-center gap-2">
        <button type="button" className="settings-row flex-1" onClick={onOpen}>
          <span className="settings-row-text">
            <b>{target.label}</b>
            <small>{line}</small>
          </span>
          {binding && live === null && <span className="chip bg-warn/10 text-warn">{t("Needs attention")}</span>}
        </button>
        {failed && !open && <Button size="sm" className="me-2" onClick={() => { void resource.refresh(); }}>{t("Retry")}</Button>}
      </div>
      {open && (
        <SettingsDrawer title={target.label} onClose={onClose}>
          <SenderCard target={target} rows={rows} hostedSmtp={hostedSmtp} viewer={viewer} onClose={onClose} />
        </SettingsDrawer>
      )}
    </>
  );
}

/** Where unsubscribe links point. Set here only to override the default. */
function OriginRow({ viewer, open, onOpen, onClose }: {
  viewer: Viewer;
  open: boolean;
  onOpen: () => void;
  onClose: () => void;
}) {
  const t = useTranslate();
  const [origin, setOrigin] = useState<string | null | undefined>(undefined);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    setFailed(false);
    sendClient.sendOriginShow().then(
      (value: unknown) => setOrigin(parseOriginReceipt(value).unsubscribeOrigin),
      () => setFailed(true),
    );
  }, [open]);
  return (
    <>
      <button type="button" className="settings-row" onClick={onOpen}>
        <span className="settings-row-text">
          <b>{t("Unsubscribe links")}</b>
          <small>{failed ? t("Couldn't read the address") : origin === undefined ? t("Reading…") : origin ?? t("Not set")}</small>
        </span>
        {origin === null && <span className="chip bg-warn/10 text-warn">{t("Needs attention")}</span>}
      </button>
      {open && (
        <SettingsDrawer title={t("Unsubscribe links")} onClose={onClose}>
          <OriginCard viewer={viewer} />
        </SettingsDrawer>
      )}
    </>
  );
}

function EmailAccounts({ headingRef, rows: accounts, complete, canAdd, hostedSmtp, onAdd }: {
  headingRef: React.Ref<HTMLHeadingElement>;
  rows: Connection[];
  complete: boolean;
  canAdd: boolean;
  hostedSmtp: boolean;
  onAdd: () => void;
}) {
  const t = useTranslate();
  return (
    <section aria-labelledby="email-accounts-title">
      <header className="flex flex-wrap items-end justify-between gap-2 mb-2">
        <div>
          <h2 ref={headingRef} id="email-accounts-title" tabIndex={-1} className="text-cardtitle font-medium text-ink-100">{t("Accounts")}</h2>
          <p className="text-label text-ink-400 mt-1">{t("Adding an account doesn’t change any app’s sender.")}</p>
        </div>
        {canAdd && <Button variant="primary" onClick={onAdd}>{t("Add email")}</Button>}
      </header>
      {accounts.length === 0 ? (
        complete && <p className="settings-rows p-3 text-label text-ink-400">{t("No email accounts connected")}</p>
      ) : (
        <ul className="settings-rows" aria-label={t("Sending accounts")}>
          {accounts.map((row) => {
            const usable = row.smtp != null && row.status.custody_available && !connectionNeedsAttention(row);
            let label = row.smtp?.sender_name || row.account;
            if (isHostedSender(row)) label = t("AgenticOS sender");
            else if (hostedSmtp && row.account === HOSTED_ACCOUNT) label = t("Workspace email account");
            return (
              <li key={row.id}>
                <div className="settings-row flex-wrap">
                  <span className="inline-flex items-center justify-center rounded bg-ink-800 text-ink-400 size-8 shrink-0" aria-hidden="true"><IconOutbox /></span>
                  <span className="settings-row-text">
                    <b>{label}</b>
                    {row.smtp?.sender && <small>{row.smtp.sender}</small>}
                  </span>
                  <span className={`chip ${complete && usable ? "bg-ok/10 text-ok" : "bg-warn/10 text-warn"}`}>
                    {!complete ? t("Not confirmed") : usable ? t("Ready") : t("Needs attention")}
                  </span>
                  {row.kind === "enrolled" && (
                    <Link
                      className="settings-quiet"
                      href={routePath({ screen: "settings", section: "connections", page: { kind: "emailAccount", id: row.id } })}
                      aria-label={t("Manage {name}").replace("{name}", () => label)}
                    >{t("Manage")}</Link>
                  )}
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

function SenderCard({
  target,
  rows,
  hostedSmtp,
  viewer,
  onClose,
}: {
  target: Target;
  rows: Connection[] | null;
  hostedSmtp: boolean;
  viewer: Viewer;
  onClose: () => void;
}) {
  const t = useTranslate();
  const selectId = useId();
  const canWrite = viewer.operator === true && !viewer.readOnly;
  const [binding, setBinding] = useState<SmtpBinding | null | undefined>(undefined);
  const [error, setError] = useState<string | null>(null);
  const [picked, setPicked] = useState("");
  const [pending, setPending] = useState(false);
  const [note, setNote] = useState<SendNote | null>(null);
  const [confirmRevoke, setConfirmRevoke] = useState(false);
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    let live = true;
    setBinding(undefined);
    setError(null);
    const resource = bindingResource(target.scope);
    void resource.refresh().then(() => {
      if (!live) return;
      const state = resource.get();
      if (state.status === "ok") setBinding(state.data);
      else setError(state.error ?? "The sender could not be loaded.");
    });
    return () => { live = false; };
  }, [target.scope.installId, target.scope.contextId, attempt]);

  const { usable, unusable } = senderChoices(rows ?? []);
  const choice = usable.find((r) => r.id === picked) ?? usable.find((r) => binding && r.id === binding.connectionId) ?? usable[0];
  const chosenId = choice?.id ?? "";

  const run = (action: () => Promise<SmtpBinding | null>, ok: SendNote) => {
    setPending(true);
    setError(null);
    setNote(null);
    void action()
      .then((value) => {
        void bindingResource(target.scope).invalidate();
        setBinding(value);
        setNote(ok);
      })
      .catch((e: unknown) => setError(friendlySendError(e)))
      .finally(() => setPending(false));
  };

  const live = sendingFrom(binding);
  return (
    <article className="border-t border-ink-700 first:border-t-0 pt-3 first:pt-0 grid gap-2 min-w-0" aria-label={target.label}>
      {binding === undefined && error === null && (
        <p className="text-label text-ink-400" role="status">{t("Reading the sender…")}</p>
      )}
      {binding === null && (
        <p className="text-label text-ink-400" data-state="unbound">
          {hostedSmtp || rows === null ? t("No sender chosen yet.") : t("No sender chosen yet. Pick one below.")}
        </p>
      )}
      {binding && live !== null && (
        <p className="text-body text-ink-100 break-words" data-state="live">
          {isHostedTransport(binding)
            ? t("Sending via AgenticOS — {live}").replace("{live}", () => live)
            : t("Sending from {live}").replace("{live}", () => live)}{" "}
          <span aria-hidden="true">✓</span>
        </p>
      )}
      {binding && live === null && (
        <p className="text-label text-fail" role="alert" data-state="stale">{rows === null ? t("The chosen sender needs re-checking") : t("The chosen sender needs re-checking. Choose it again below to continue sending.")}</p>
      )}
      {error && (
        <p className="text-label text-fail break-words" role="alert">
          {error}{" "}
          <Button size="sm" disabled={pending} onClick={() => setAttempt((n) => n + 1)}>{t("Retry")}</Button>
        </p>
      )}
      {note && (
        <p className="text-label text-ok" role="status">
          {renderNote(note, t)}
        </p>
      )}
      {unusable.length > 0 && (
        <ul className="grid gap-1" aria-label={t("Senders that cannot be used")}>
          {unusable.map(({ row, reason }) => (
            <li key={row.id} className="text-label text-fail break-words" data-state="sender-unusable">
              {t("{account} can't be used yet. {reason}").replace("{account}", () => String(row.account ?? "")).replace("{reason}", () => String(reason ?? ""))} <Link href={EMAIL_CONNECTIONS_HREF} onClick={onClose}>{t("Manage email accounts")}</Link>.
            </li>
          ))}
        </ul>
      )}
      {rows !== null && usable.length === 0 && (
        <p className="text-label text-ink-500">
          {hostedSmtp ? t("No workspace email account is available.") : t("No email sender is available.")}{" "}
          <Link href={EMAIL_CONNECTIONS_HREF} onClick={onClose}>{t("Manage email accounts")}</Link>.
        </p>
      )}
      {canWrite && error === null && usable.length > 0 && (
        <div className="crm-field-row">
          <div className="crm-field">
            <label className="text-label text-ink-300" htmlFor={selectId}>{t("Send email from")}</label>
            <Select
              id={selectId}
              value={chosenId}
              onChange={setPicked}
              options={usable.map((r) => ({ value: r.id, label: senderLine(r) }))}
              aria-label={t("Send email from")}
              disabled={pending}
              full
            />
          </div>
          <div className="crm-toolbar" style={{ alignSelf: "end" }}>
            {binding === null ? (
              <Button
                size="sm"
                variant="primary"
                loading={pending}
                disabled={pending || chosenId === ""}
                onClick={() =>
                  run(
                    () => sendClient.smtpBind(target.scope, chosenId, newAudienceId("bind")).then(parseSmtpBinding),
                    { key: "chosen" },
                  )
                }
              >{t("Use this sender")}</Button>
            ) : (
              binding && (
                <>
                  {(live === null || chosenId !== binding.connectionId) && (
                  <Button
                    size="sm"
                    variant="primary"
                    loading={pending}
                    disabled={pending || chosenId === "" || (live !== null && chosenId === binding.connectionId)}
                    onClick={() =>
                      run(
                        () => sendClient.smtpRebind(target.scope, chosenId, binding.linkRevision).then(parseSmtpBinding),
                        { key: "changed" },
                      )
                    }
                  >
                    {t("Switch to this sender")}
                  </Button>
                  )}
                  <Button size="sm" variant="danger" disabled={pending} onClick={() => setConfirmRevoke(true)}>{t("Stop using")}</Button>
                </>
              )
            )}
          </div>
        </div>
      )}
      {binding && (
        <details className="text-label text-ink-400" data-details="sender">
          <summary className="cursor-pointer">{t("Details")}</summary>
          <p className="num break-all mt-1">
            {t("link r{linkRevision} · auth r{authRevision} · {state} ·").replace("{linkRevision}", String(binding.linkRevision ?? "")).replace("{authRevision}", String(binding.authRevision ?? "")).replace("{state}", () => String(binding.state ?? ""))} {binding.digest.slice(0, 18)}…
          </p>
        </details>
      )}
      {confirmRevoke && binding && rows !== null && canWrite && error === null && (
        <ConfirmDialog
          title={t("Stop using this sender?")}
          body={
            <p>
              {t("This app can't send or test-send from {address} until a sender is chosen again. A prepared send loses its approval.").replace("{address}", () => String(binding.sender.address ?? ""))}</p>
          }
          confirmLabel={t("Stop using")}
          pending={pending}
          error={null}
          onCancel={() => setConfirmRevoke(false)}
          onConfirm={() => {
            setConfirmRevoke(false);
            run(async () => {
              await sendClient.smtpRevoke(target.scope, binding.linkRevision);
              return null;
            }, { key: "removed" });
          }}
        />
      )}
    </article>
  );
}

/** The daemon-wide unsubscribe origin — set once; every campaign uses it. */
function OriginCard({ viewer }: { viewer: Viewer }) {
  const t = useTranslate();
  const canWrite = viewer.operator === true && !viewer.readOnly;
  const [origin, setOrigin] = useState<string | null | undefined>(undefined);
  const [stored, setStored] = useState(false);
  const [input, setInput] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<SendNote | null>(null);

  const apply = (value: unknown) => {
    const receipt = parseOriginReceipt(value);
    setOrigin(receipt.unsubscribeOrigin);
    setStored(receipt.stored);
    // The field holds only your own override: saving the default would pin it.
    setInput(receipt.stored ? receipt.unsubscribeOrigin ?? "" : "");
    return receipt;
  };
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    setError(null);
    sendClient.sendOriginShow().then(apply, (e: unknown) => setError(friendlySendError(e)));
  }, [attempt]);

  const save = (value: string | null) => {
    setPending(true);
    setError(null);
    setNote(null);
    // Re-read after writing: only the read says what is now stored, on every board.
    void Promise.resolve()
      .then(() => sendClient.sendOriginSet(value))
      .then(() =>
        // The write landed. A failed read mustn't report it as failed or show the old value.
        sendClient.sendOriginShow().catch(() => ({ unsubscribe_origin: value, stored: value !== null })),
      )
      .then((shown) => {
        const receipt = apply(shown);
        setNote(
          value === null || receipt.unsubscribeOrigin === null
            ? { key: "origin-cleared" }
            : { key: "origin-set", value: receipt.unsubscribeOrigin },
        );
      })
      .catch((e: unknown) => setError(friendlySendError(e)))
      .finally(() => setPending(false));
  };

  return (
    <section aria-label={t("Unsubscribe origin")} className="card px-4 py-4 grid gap-3 min-w-0">
      
      {origin === undefined && error === null ? (
        <p className="text-label text-ink-400" role="status">{t("Reading…")}</p>
      ) : (
        <p className="text-label text-ink-300 break-words">
          {t("Every email's unsubscribe link is built on")}{" "}
          <span className="num">{origin ?? t("nothing — sends refuse until one is set")}</span>
          {stored ? ` ${t("(set here)")}` : origin ? ` ${t("(server default)")}` : ""}.
        </p>
      )}
      {error && (
        <p className="text-label text-fail break-words" role="alert">
          {error}{" "}
          {origin === undefined && (
            <Button size="sm" onClick={() => setAttempt((n) => n + 1)}>{t("Retry")}</Button>
          )}
        </p>
      )}
      {note && (
        <p className="text-label text-ok" role="status">
          {renderNote(note, t)}
        </p>
      )}
      {canWrite && (
        <form
          className="crm-field-row"
          onSubmit={(e) => {
            e.preventDefault();
            if (input.trim() !== "") save(input);
          }}
        >
          <div className="crm-field">
            <label className="text-label text-ink-300" htmlFor="email-origin">{t("Use a different address (optional)")}</label>
            <input
              id="email-origin"
              className="field"
              value={input}
              onChange={(e) => setInput(e.target.value)}
              maxLength={200}
              autoComplete="off"
              disabled={pending}
              placeholder="https://cadence.example.com"
            />
          </div>
          <div className="crm-toolbar" style={{ alignSelf: "end" }}>
            {input.trim() !== "" && input.trim() !== (stored ? origin ?? "" : "") && (
              <Button type="submit" size="sm" loading={pending} disabled={pending}>{t("Save")}</Button>
            )}
            {stored && (
              <Button size="sm" disabled={pending} onClick={() => save(null)}>{t("Clear")}</Button>
            )}
          </div>
        </form>
      )}
    </section>
  );
}
