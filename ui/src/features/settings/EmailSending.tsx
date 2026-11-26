import { useEffect, useId, useRef, useState } from "react";
import { api } from "../../lib/api";
import { resources } from "../../lib/resources";
import { EMAIL_CONNECTIONS_HREF, routePath } from "../../lib/router";
import type { Connection } from "../../lib/types";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import Select from "../../ui/Select";
import PageState from "../../ui/PageState";
import { IconOutbox } from "../../ui/icons";
import type { Viewer } from "../projects/work";
import ConfirmDialog from "../app-shell/shared/ConfirmDialog";
import SettingsDrawer from "./SettingsDrawer";
import EmailSetup from "./EmailSetup";
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
import { isSmtpSender } from "./connectionsView";
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
async function loadTargets(signal: AbortSignal): Promise<Target[]> {
  const installs = (await workspaceApps.installations(signal)).filter(
    (i) => i.smtp_custody === true && activeInstall(i),
  );
  const out: Target[] = [];
  for (const install of installs) {
    const contexts = await workspaceApps.contexts(install.install_id, signal);
    for (const ctx of contexts.filter((c) => c.state === "active")) {
      out.push({
        scope: { installId: install.install_id, contextId: ctx.id },
        label: appContextLabel(install.title || install.name, ctx.config.label || ctx.id),
      });
    }
  }
  return out;
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
  const [targets, setTargets] = useState<Target[] | null>(null);
  const [rows, setRows] = useState<Connection[] | null>(null);
  const [hostedSmtp, setHostedSmtp] = useState(false);
  const [accountsError, setAccountsError] = useState<string | null>(null);
  const [targetsError, setTargetsError] = useState<string | null>(null);
  const [partial, setPartial] = useState(false);
  const [accountsAttempt, setAccountsAttempt] = useState(0);
  const [targetsAttempt, setTargetsAttempt] = useState(0);
  // One drawer at a time: rows stay reachable by keyboard behind an open one.
  const [openRow, setOpenRow] = useState<string | null>(null);
  const sendersHeading = useRef<HTMLHeadingElement>(null);
  const accountsHeading = useRef<HTMLHeadingElement>(null);
  const accountAdded = useRef(false);
  const sendersFocused = useRef(false);
  const accountsLoaded = rows !== null;
  useEffect(() => {
    if (viewer.operator !== true) return;
    if (accountAdded.current && accountsLoaded) {
      accountAdded.current = false;
      accountsHeading.current?.focus();
    } else if (!accountAdded.current && focusSenders && !sendersFocused.current) {
      sendersHeading.current?.focus();
      sendersFocused.current = true;
    }
  }, [focusSenders, accountsLoaded, viewer.operator]);

  const canAdd = viewer.operator === true && !viewer.readOnly && rows !== null && !partial
    && !(hostedSmtp && rows.some((row) => row.provider === "smtp" && row.account === HOSTED_ACCOUNT));

  useEffect(() => {
    if (viewer.operator !== true) return;
    const controller = new AbortController();
    api.connections().then(
      (list) => {
        if (controller.signal.aborted) return;
        setRows(list.connections ?? []);
        setHostedSmtp(list.hosted_smtp === true);
        setPartial(list.connections_unavailable !== undefined);
        setAccountsError(null);
      },
      (e: unknown) => {
        if (!controller.signal.aborted) setAccountsError(friendlySendError(e));
      },
    );
    return () => controller.abort();
  }, [viewer.operator, accountsAttempt]);

  useEffect(() => {
    if (viewer.operator !== true) return;
    const controller = new AbortController();
    loadTargets(controller.signal).then(
      (nextTargets) => {
        if (controller.signal.aborted) return;
        setTargets(nextTargets);
        setTargetsError(null);
      },
      (e: unknown) => {
        if (!controller.signal.aborted) setTargetsError(friendlySendError(e));
      },
    );
    return () => controller.abort();
  }, [viewer.operator, targetsAttempt]);

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
          <EmailAccounts headingRef={accountsHeading} rows={rows ?? []} complete={rows !== null && !partial && accountsError === null} canAdd={canAdd} hostedSmtp={hostedSmtp} onAdd={() => setOpenRow("add")} />
          {accountsError !== null ? (
            <p className="text-label text-fail flex flex-wrap items-center gap-2" role="alert">
              {accountsError}
              <Button size="sm" onClick={() => { setAccountsError(null); setRows(null); setAccountsAttempt((n) => n + 1); }}>{t("Retry")}</Button>
            </p>
          ) : rows === null ? (
            <p className="text-label text-ink-400" role="status">{t("Loading…")}</p>
          ) : partial ? (
            <p className="text-label text-fail flex flex-wrap items-center gap-2" role="alert" data-state="connections-partial">
              {t("Some connected accounts couldn't be loaded, so this list may be incomplete.")}
              <Button size="sm" onClick={() => { setRows(null); setAccountsAttempt((n) => n + 1); }}>{t("Retry")}</Button>
            </p>
          ) : null}
          <section aria-labelledby="app-senders">
            <h2 ref={sendersHeading} id="app-senders" tabIndex={-1} className="text-cardtitle font-medium text-ink-100">{t("App senders")}</h2>
            <p className="text-label text-ink-400 mt-1 mb-2">{t("Choose the account each app sends from.")}</p>
            {targetsError !== null ? (
              <p className="text-label text-fail flex flex-wrap items-center gap-2" role="alert">
                {targetsError}
                <Button size="sm" onClick={() => { setTargetsError(null); setTargets(null); setTargetsAttempt((n) => n + 1); }}>{t("Retry")}</Button>
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
                    rows={rows !== null && !partial && accountsError === null ? rows : null}
                    hostedSmtp={hostedSmtp}
                    viewer={viewer}
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
                setRows(null);
                setAccountsAttempt((n) => n + 1);
                void resources.connections.invalidate();
              }}
            />
          )}
        </div>
      )}
    </section>
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
  const [binding, setBinding] = useState<SmtpBinding | null | undefined>(undefined);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    const controller = new AbortController();
    setFailed(false);
    readSmtpBinding(target.scope).then(
      (bound) => {
        if (!controller.signal.aborted) setBinding(bound);
      },
      () => {
        if (!controller.signal.aborted) setFailed(true);
      },
    );
    return () => controller.abort();
  }, [target.scope.installId, target.scope.contextId, open]);
  const live = sendingFrom(binding);
  const line = failed
    ? t("Couldn't read the sender")
    : binding === undefined
      ? t("Reading…")
      : binding === null
        ? t("No sender chosen")
        : live ?? t("The chosen sender needs re-checking");
  return (
    <>
      <button type="button" className="settings-row" onClick={onOpen}>
        <span className="settings-row-text">
          <b>{target.label}</b>
          <small>{line}</small>
        </span>
        {binding && live === null && <span className="chip bg-warn/10 text-warn">{t("Needs attention")}</span>}
      </button>
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

function EmailAccounts({ headingRef, rows, complete, canAdd, hostedSmtp, onAdd }: {
  headingRef: React.Ref<HTMLHeadingElement>;
  rows: Connection[];
  complete: boolean;
  canAdd: boolean;
  hostedSmtp: boolean;
  onAdd: () => void;
}) {
  const t = useTranslate();
  const accounts = rows.filter(isSmtpSender);
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
            const usable = row.smtp != null && row.status.custody_available;
            let label = row.account;
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
                  <span className={`chip ${usable ? "bg-ok/10 text-ok" : "bg-warn/10 text-warn"}`}>{usable ? t("Ready") : t("Needs attention")}</span>
                  {row.kind === "enrolled" && (
                    <Link
                      className="settings-quiet"
                      href={routePath({ screen: "settings", section: "connections", page: { kind: "detail", id: row.id } })}
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
    const controller = new AbortController();
    setBinding(undefined);
    setError(null);
    readSmtpBinding(target.scope).then(
      (bound) => {
        if (!controller.signal.aborted) setBinding(bound);
      },
      (e: unknown) => {
        if (!controller.signal.aborted) setError(friendlySendError(e));
      },
    );
    return () => controller.abort();
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
          {binding === undefined && (
            <Button size="sm" onClick={() => setAttempt((n) => n + 1)}>{t("Retry")}</Button>
          )}
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
      {canWrite && usable.length > 0 && (
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
      {confirmRevoke && binding && (
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
