import { useEffect, useId, useState } from "react";
import { api } from "../../lib/api";
import type { Connection } from "../../lib/types";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import Select from "../../ui/Select";
import PageState from "../../ui/PageState";
import type { Viewer } from "../projects/work";
import ConfirmDialog from "../app-shell/shared/ConfirmDialog";
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
import { HOSTED_ACCOUNT } from "./hostedSmtpView";
import { workspaceApps, type Installation } from "../workspace-apps/workspaceApps";
import {
  isHostedTransport,
  senderChoices,
  senderLine,
  sendingFrom,
} from "./emailSendingView";
import { useTranslate } from "../../lib/locale";

/**
 * Settings → Email sending (CAD-1059, CAD-1063 slice 2, CAD-1797): the
 * workspace-wide email policy. Every installed app/context whose bundle
 * declares `smtp.bind` is listed with its current sender and status; each
 * one keeps its own binding. The accounts themselves live in Connections
 * — this page never duplicates the add/replace SMTP form.
 */

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
        label: `${install.title || install.name} · ${ctx.config.label || ctx.id}`,
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
    case "origin-cleared": return t("Unsubscribe address cleared — sends refuse until one is set.");
    case "origin-set": return t("Unsubscribe links now use {origin}.").replace("{origin}", () => note.value ?? "");
  }
}

export default function EmailSending({ viewer }: { viewer: Viewer }) {
  const t = useTranslate();
  const [targets, setTargets] = useState<Target[] | null>(null);
  const [rows, setRows] = useState<Connection[] | null>(null);
  const [hostedSmtp, setHostedSmtp] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    // CAD-1193: unresolved role (null) waits; only a proven operator
    // issues the sender-target and account reads.
    if (viewer.operator !== true) return;
    const controller = new AbortController();
    Promise.all([loadTargets(controller.signal), api.connections()]).then(
      ([nextTargets, list]) => {
        if (controller.signal.aborted) return;
        setTargets(nextTargets);
        setRows(list.connections ?? []);
        setHostedSmtp(list.hosted_smtp === true);
        setError(null);
      },
      (e: unknown) => {
        if (!controller.signal.aborted) setError(friendlySendError(e));
      },
    );
    return () => controller.abort();
  }, [viewer.operator]);

  return (
    <section className="px-4 lg:px-8 py-5 min-w-0" aria-labelledby="email-sending-title">
      <h1 id="email-sending-title" className="text-section font-medium text-ink-100">{t("Email sending")}</h1>
      <p className="text-label text-ink-400 mt-1 mb-4 break-words">{t("Choose the account your apps send email from. An app can use the workspace account or a sender of its own.")}</p>
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
      ) : error !== null ? (
        <p className="text-label text-fail" role="alert">
          {error}
        </p>
      ) : targets === null || rows === null ? (
        <p className="text-label text-ink-400" role="status">{t("Loading…")}</p>
      ) : (
        <div className="grid gap-3 min-w-0">
          <WorkspaceAccountCard rows={rows} hostedSmtp={hostedSmtp} />
          {targets.length === 0 ? (
            <p className="text-label text-ink-400" data-state="no-apps">{t("No installed app sends email, so there is nothing to set up yet.")}</p>
          ) : (
            <section className="card px-4 py-4 grid gap-3 min-w-0" aria-labelledby="email-apps-title">
              <div>
                <h2 id="email-apps-title" className="text-cardtitle font-medium text-ink-100">{t("Apps that send email")}</h2>
                <p className="text-label text-ink-400 mt-1 break-words">{t("Each app can use the workspace account or a sender of its own. An app appears here when its screens declare email sending.")}</p>
              </div>
              <div className="grid gap-2 min-w-0">
                {targets.map((target) => (
                  <SenderCard
                    key={`${target.scope.installId}:${target.scope.contextId}`}
                    target={target}
                    rows={rows}
                    hostedSmtp={hostedSmtp}
                    viewer={viewer}
                  />
                ))}
              </div>
            </section>
          )}
          <OriginCard viewer={viewer} />
        </div>
      )}
    </section>
  );
}

/** The one workspace account: stored once in Connections, chosen per app. */
function WorkspaceAccountCard({ rows, hostedSmtp }: { rows: Connection[]; hostedSmtp: boolean }) {
  const t = useTranslate();
  const email = hostedSmtp
    ? rows.find((r) => r.provider === "smtp" && r.account === HOSTED_ACCOUNT) ?? null
    : null;
  if (!hostedSmtp) {
    return (
      <section className="card px-4 py-4 grid gap-2 min-w-0" aria-labelledby="email-accounts-title">
        <div className="flex flex-wrap items-start gap-x-3 gap-y-1">
          <div className="min-w-0 flex-1">
            <p className="text-micro uppercase tracking-wide text-ink-500">{t("Accounts")}</p>
            <h2 id="email-accounts-title" className="text-cardtitle font-medium text-ink-100">{t("Email accounts")}</h2>
            <p className="text-label text-ink-300 mt-1 break-words">{t("Email accounts are managed in Connections. Each app chooses the account it sends from.")}</p>
          </div>
          <Link href="/settings/connections" className="lnk text-label shrink-0">{t("Manage in Connections")}</Link>
        </div>
      </section>
    );
  }
  const address = email?.smtp?.sender ?? null;
  const unreadable = email !== null && (email.smtp ?? null) === null;
  return (
    <section className="card px-4 py-4 grid gap-2 min-w-0" aria-labelledby="workspace-account-title">
      <div className="flex flex-wrap items-start gap-x-3 gap-y-1">
        <div className="min-w-0 flex-1">
          <p className="text-micro uppercase tracking-wide text-ink-500">{t("Sending account")}</p>
          <h2 id="workspace-account-title" className="text-cardtitle font-medium text-ink-100">{t("Workspace email account")}</h2>
          <p className="text-body text-ink-100 mt-1 break-words" data-state={address ? "connected" : unreadable ? "unreadable" : "none"}>
            {address ?? (unreadable ? t("The saved email account can't be read. Replace it in Connections.") : t("No workspace email account connected yet."))}
          </p>
          <p className="text-label text-ink-400 mt-1 break-words">{t("Apps can use this account or choose another connected account.")}</p>
        </div>
        <Link href="/settings/connections" className="lnk text-label shrink-0">
          {address || unreadable ? t("Manage in Connections") : t("Add one in Connections")}
        </Link>
      </div>
    </section>
  );
}

function SenderCard({
  target,
  rows,
  hostedSmtp,
  viewer,
}: {
  target: Target;
  rows: Connection[];
  hostedSmtp: boolean;
  viewer: Viewer;
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
  }, [target.scope.installId, target.scope.contextId]);

  const { usable, unusable } = senderChoices(rows);
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
      <div className="flex flex-wrap items-start gap-x-3 gap-y-1">
        <div className="min-w-0 flex-1">
          <h3 className="text-body font-medium text-ink-100 break-words">{target.label}</h3>
          <p className="text-micro text-ink-500 break-words">{target.scope.installId} · {target.scope.contextId}</p>
        </div>
        {binding === undefined && error === null ? (
          <span className="chip bg-ink-800 text-ink-300">{t("Reading…")}</span>
        ) : binding && live !== null ? (
          <span className="chip bg-ok/10 text-ok">{t("Connected")}</span>
        ) : binding && live === null ? (
          <span className="chip bg-warn/10 text-warn">{t("Needs re-check")}</span>
        ) : (
          <span className="chip bg-ink-800 text-ink-300">{t("Not configured")}</span>
        )}
      </div>
      {binding === undefined && error === null && (
        <p className="text-label text-ink-400" role="status">{t("Reading the sender…")}</p>
      )}
      {binding === null && (
        <p className="text-label text-ink-400" data-state="unbound">
          {hostedSmtp ? t("No sender chosen yet.") : t("No sender chosen yet. Pick one below.")}
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
        <p className="text-label text-fail" role="alert" data-state="stale">{t("The chosen sender needs re-checking. Choose it again below to continue sending.")}</p>
      )}
      {error && (
        <p className="text-label text-fail break-words" role="alert">
          {error}
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
              {t("{account} can't be used yet. {reason}").replace("{account}", () => String(row.account ?? "")).replace("{reason}", () => String(reason ?? ""))} <Link href="/settings/connections">{t("Fix it in Settings → Connections")}</Link>.
            </li>
          ))}
        </ul>
      )}
      {usable.length === 0 && (
        <p className="text-label text-ink-500">
          {hostedSmtp ? t("No workspace email account is available.") : t("No email sender is available.")}{" "}
          <Link href="/settings/connections">{t("Add one in Settings → Connections")}</Link>.
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
                    {live === null || chosenId !== binding.connectionId ? t("Switch to this sender") : t("Current sender")}
                  </Button>
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
    setInput(receipt.unsubscribeOrigin ?? "");
    return receipt;
  };
  useEffect(() => {
    sendClient.sendOriginShow().then(apply, (e: unknown) => setError(friendlySendError(e)));
  }, []);

  const save = (value: string | null) => {
    setPending(true);
    setError(null);
    setNote(null);
    void sendClient
      .sendOriginSet(value)
      .then((value2) => {
        const receipt = apply(value2);
        setNote(
          receipt.unsubscribeOrigin === null
            ? { key: "origin-cleared" }
            : { key: "origin-set", value: receipt.unsubscribeOrigin },
        );
      })
      .catch((e: unknown) => setError(friendlySendError(e)))
      .finally(() => setPending(false));
  };

  return (
    <section aria-label={t("Unsubscribe origin")} className="card px-4 py-4 grid gap-3 min-w-0">
      <h2 className="text-cardtitle font-medium text-ink-100">{t("Unsubscribe address")}</h2>
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
          {error}
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
            save(input);
          }}
        >
          <div className="crm-field">
            <label className="text-label text-ink-300" htmlFor="email-origin">{t("Origin (https; http on a loopback host)")}</label>
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
            <Button type="submit" size="sm" loading={pending} disabled={pending}>{t("Save")}</Button>
            {stored && (
              <Button size="sm" disabled={pending} onClick={() => save(null)}>{t("Clear")}</Button>
            )}
          </div>
        </form>
      )}
    </section>
  );
}
