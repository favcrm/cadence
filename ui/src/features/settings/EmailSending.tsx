import { useEffect, useState } from "react";
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
import HostedSmtpConnect, { type ConnectDetails } from "./HostedSmtpConnect";
import { HOSTED_ACCOUNT, SMTP_SEND_SCOPE } from "./hostedSmtpView";
import { workspaceApps } from "../workspace-apps/workspaceApps";
import {
  isHostedTransport,
  senderChoices,
  senderLine,
  sendingFrom,
} from "./emailSendingView";

/**
 * Settings → Email sending (CAD-1059, CAD-1063 slice 2): the one place
 * the CRM sender and the unsubscribe origin are chosen. Campaigns only
 * show a status line and link here. The write verbs are the existing
 * crm_smtp bind/rebind/revoke; link and auth revisions stay behind the
 * Details disclosure and never reach a form.
 */

interface Target {
  scope: HostScope;
  label: string;
}

/** Every active CRM context, flattened. One entry means no selector. */
async function loadTargets(signal: AbortSignal): Promise<Target[]> {
  const installs = (await workspaceApps.installations(signal)).filter((i) => i.name === "crm");
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

export default function EmailSending({ viewer }: { viewer: Viewer }) {
  const [targets, setTargets] = useState<Target[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [picked, setPicked] = useState(0);

  useEffect(() => {
    // CAD-1193: unresolved role (null) waits; only a proven operator
    // issues the sender-target read.
    if (viewer.operator !== true) return;
    const controller = new AbortController();
    loadTargets(controller.signal).then(
      (rows) => {
        if (!controller.signal.aborted) setTargets(rows);
      },
      (e: unknown) => {
        if (!controller.signal.aborted) setError(friendlySendError(e));
      },
    );
    return () => controller.abort();
  }, [viewer.operator]);

  const target = targets?.[picked] ?? null;
  return (
    <section className="px-4 lg:px-8 py-5 min-w-0" aria-labelledby="email-sending-title">
      <h1 id="email-sending-title" className="text-section font-medium text-ink-100">
        Email sending
      </h1>
      <p className="text-label text-ink-400 mt-1 mb-4 break-words">
        Choose where CRM campaign email is sent from. Campaigns use this sender; nothing is sent
        until an operator approves a campaign.
      </p>
      {viewer.operator === null ? (
        <PageState title={viewer.access === "unavailable" ? "Access could not be confirmed" : "Checking access…"}>
          {viewer.access === "unavailable" ? (
            <>
              The board's access check did not answer, so email sending stays closed.{" "}
              {viewer.onRetryAccess && (
                <Button size="sm" onClick={viewer.onRetryAccess}>
                  Retry access check
                </Button>
              )}
            </>
          ) : (
            "Checking whether this session may manage email sending…"
          )}
        </PageState>
      ) : !viewer.operator ? (
        <PageState title="Operator access required">
          Email sending is available to the operator. Use Sign in in the status bar if you
          have operator access.
        </PageState>
      ) : error !== null ? (
        <p className="text-label text-fail" role="alert">
          {error}
        </p>
      ) : targets === null ? (
        <p className="text-label text-ink-400" role="status">
          Loading…
        </p>
      ) : target === null ? (
        <p className="text-label text-ink-400" data-state="no-crm">
          No CRM is installed in this workspace, so there is nothing to send from yet.
        </p>
      ) : (
        <div className="grid gap-3 min-w-0">
          {targets.length > 1 && (
            <div className="crm-field">
              <label className="text-label text-ink-300" htmlFor="email-sending-target">
                CRM context
              </label>
              <Select
                id="email-sending-target"
                value={String(picked)}
                onChange={(v) => setPicked(Number(v))}
                options={targets.map((t, i) => ({ value: String(i), label: t.label }))}
                aria-label="CRM context"
                full
              />
            </div>
          )}
          <SenderCard key={`${target.scope.installId}:${target.scope.contextId}`} scope={target.scope} viewer={viewer} />
          <OriginCard viewer={viewer} />
        </div>
      )}
    </section>
  );
}

function SenderCard({ scope, viewer }: { scope: HostScope; viewer: Viewer }) {
  const canWrite = viewer.operator === true && !viewer.readOnly;
  const [rows, setRows] = useState<Connection[] | null>(null);
  const [hostedSmtp, setHostedSmtp] = useState(false);
  const [replacing, setReplacing] = useState(false);
  const [binding, setBinding] = useState<SmtpBinding | null | undefined>(undefined);
  const [error, setError] = useState<string | null>(null);
  const [picked, setPicked] = useState("");
  const [pending, setPending] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [confirmRevoke, setConfirmRevoke] = useState(false);

  useEffect(() => {
    const controller = new AbortController();
    Promise.all([api.connections(), readSmtpBinding(scope)]).then(
      ([list, bound]) => {
        if (controller.signal.aborted) return;
        setRows(list.connections ?? []);
        setHostedSmtp(list.hosted_smtp === true);
        setBinding(bound);
        setError(null);
      },
      (e: unknown) => {
        if (!controller.signal.aborted) setError(friendlySendError(e));
      },
    );
    return () => controller.abort();
  }, [scope.installId, scope.contextId]);

  const { usable, unusable } = senderChoices(rows ?? []);
  const choice = usable.find((r) => r.id === picked) ?? usable.find((r) => binding && r.id === binding.connectionId) ?? usable[0];
  const chosenId = choice?.id ?? "";

  const run = (action: () => Promise<SmtpBinding | null>, ok: string) => {
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

  const emailRow = rows?.find((r) => r.provider === "smtp" && r.account === HOSTED_ACCOUNT) ?? null;
  const emailAddress = emailRow?.smtp?.sender ?? null;

  // CAD-1126: connect (or replace) the tenant's own SMTP and bind it as
  // the CRM sender in one step. A refusal from the verify step throws to
  // the form, which shows plain words; a bind failure shows here.
  const connectEmail = async (d: ConnectDetails, acceptRisk: boolean) => {
    const fields = {
      host: d.host,
      port: d.port,
      tls_mode: d.tls_mode,
      username: d.username,
      secret: d.password,
      sender: d.sender,
      ...(d.sender_name ? { sender_name: d.sender_name } : {}),
      // Only after the daemon refused with custody_unprotected and the
      // operator ticked the honest consent (CAD-1013 pattern).
      ...(acceptRisk ? { accept_same_uid_risk: true } : {}),
    };
    const out = emailRow
      ? await api.connectionRotate(emailRow.id, fields)
      : await api.connectionCreate({
          provider: "smtp",
          account: HOSTED_ACCOUNT,
          shape: "smtp",
          scopes: [SMTP_SEND_SCOPE],
          ...fields,
        });
    setReplacing(false);
    setError(null);
    try {
      const [list, fresh] = await Promise.all([api.connections(), readSmtpBinding(scope)]);
      setRows(list.connections ?? []);
      const id = out.connection.id;
      const bound = fresh
        ? await sendClient.smtpRebind(scope, id, fresh.linkRevision)
        : await sendClient.smtpBind(scope, id, newAudienceId("bind"));
      setBinding(parseSmtpBinding(bound));
      setNote(`Connected. Campaigns now send from ${d.sender}.`);
    } catch (e: unknown) {
      setError(friendlySendError(e));
    }
  };

  const live = sendingFrom(binding);
  return (
    <section aria-label="Sender" className="card px-4 py-4 grid gap-3 min-w-0">
      <h2 className="text-cardtitle font-medium text-ink-100">Sender</h2>
      {hostedSmtp && canWrite && rows !== null && (
        <div className="grid gap-2 min-w-0" data-section="connect-email">
          <h3 className="text-body font-medium text-ink-100">Connect your email (SMTP)</h3>
          {emailRow && !replacing ? (
            <div className="grid gap-2">
              <p className="text-label text-ink-300 break-words" data-state="email-connected">
                Connected: {emailAddress ?? emailRow.account}
              </p>
              <div className="crm-toolbar">
                <Button size="sm" onClick={() => setReplacing(true)}>
                  Replace email account
                </Button>
              </div>
            </div>
          ) : (
            <>
              <p className="text-label text-ink-400 break-words">
                Send campaigns from your own email account. Pick your provider, then paste an
                app password. We check the login before saving anything.
              </p>
              <HostedSmtpConnect
                connected={emailAddress}
                onConnect={connectEmail}
                onCancel={emailRow ? () => setReplacing(false) : undefined}
              />
            </>
          )}
        </div>
      )}
      {binding === undefined && error === null && (
        <p className="text-label text-ink-400" role="status">
          Reading the sender…
        </p>
      )}
      {binding === null && (
        <p className="text-label text-ink-400" data-state="unbound">
          {hostedSmtp ? "No sender chosen yet." : "No sender chosen yet. Pick one below."}
        </p>
      )}
      {binding && live !== null && (
        <p className="text-body text-ink-100 break-words" data-state="live">
          {isHostedTransport(binding)
            ? `Sending via AgenticOS — ${live}`
            : `Sending from ${live}`}{" "}
          <span aria-hidden="true">✓</span>
        </p>
      )}
      {binding && live === null && (
        <p className="text-label text-fail" role="alert" data-state="stale">
          The chosen sender needs re-checking. Choose it again below to continue sending.
        </p>
      )}
      {error && (
        <p className="text-label text-fail break-words" role="alert">
          {error}
        </p>
      )}
      {note && (
        <p className="text-label text-ok" role="status">
          {note}
        </p>
      )}
      {rows !== null && unusable.length > 0 && (
        <ul className="grid gap-1" aria-label="Senders that cannot be used">
          {unusable.map(({ row, reason }) => (
            <li key={row.id} className="text-label text-fail break-words" data-state="sender-unusable">
              {row.account} can't be used yet. {reason}{" "}
              <Link href="/settings/connections">Fix it in Settings → Connections</Link>.
            </li>
          ))}
        </ul>
      )}
      {rows !== null && !hostedSmtp && usable.length === 0 && unusable.length === 0 && (
        <p className="text-label text-ink-500">
          No email sender is available. Add an SMTP sender under{" "}
          <Link href="/settings/connections">Settings → Connections</Link>.
        </p>
      )}
      {canWrite && usable.length > 0 && !(hostedSmtp && usable.length === 1 && emailRow && live !== null) && (
        <div className="crm-field-row">
          <div className="crm-field">
            <label className="text-label text-ink-300" htmlFor="email-sender">
              Send campaign email from
            </label>
            <Select
              id="email-sender"
              value={chosenId}
              onChange={setPicked}
              options={usable.map((r) => ({ value: r.id, label: senderLine(r) }))}
              aria-label="Send campaign email from"
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
                    () => sendClient.smtpBind(scope, chosenId, newAudienceId("bind")).then(parseSmtpBinding),
                    "Sender chosen.",
                  )
                }
              >
                Use this sender
              </Button>
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
                        () => sendClient.smtpRebind(scope, chosenId, binding.linkRevision).then(parseSmtpBinding),
                        "Sender changed.",
                      )
                    }
                  >
                    {live === null || chosenId !== binding.connectionId ? "Switch to this sender" : "Current sender"}
                  </Button>
                  <Button size="sm" variant="danger" disabled={pending} onClick={() => setConfirmRevoke(true)}>
                    Stop using
                  </Button>
                </>
              )
            )}
          </div>
        </div>
      )}
      {binding && (
        <details className="text-label text-ink-400" data-details="sender">
          <summary className="cursor-pointer">Details</summary>
          <p className="num break-all mt-1">
            link r{binding.linkRevision} · auth r{binding.authRevision} · {binding.state} ·{" "}
            {binding.digest.slice(0, 18)}…
          </p>
        </details>
      )}
      {confirmRevoke && binding && (
        <ConfirmDialog
          title="Stop using this sender?"
          body={
            <p>
              Campaigns can't send or test-send from {binding.sender.address} until a sender is
              chosen again. A prepared send loses its approval.
            </p>
          }
          confirmLabel="Stop using"
          pending={pending}
          error={null}
          onCancel={() => setConfirmRevoke(false)}
          onConfirm={() => {
            setConfirmRevoke(false);
            run(async () => {
              await sendClient.smtpRevoke(scope, binding.linkRevision);
              return null;
            }, "Sender removed.");
          }}
        />
      )}
    </section>
  );
}

/** The daemon-wide unsubscribe origin — set once; every campaign uses it. */
function OriginCard({ viewer }: { viewer: Viewer }) {
  const canWrite = viewer.operator === true && !viewer.readOnly;
  const [origin, setOrigin] = useState<string | null | undefined>(undefined);
  const [stored, setStored] = useState(false);
  const [input, setInput] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

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
            ? "Unsubscribe address cleared — sends refuse until one is set."
            : `Unsubscribe links now use ${receipt.unsubscribeOrigin}.`,
        );
      })
      .catch((e: unknown) => setError(friendlySendError(e)))
      .finally(() => setPending(false));
  };

  return (
    <section aria-label="Unsubscribe origin" className="card px-4 py-4 grid gap-3 min-w-0">
      <h2 className="text-cardtitle font-medium text-ink-100">Unsubscribe address</h2>
      {origin === undefined && error === null ? (
        <p className="text-label text-ink-400" role="status">
          Reading…
        </p>
      ) : (
        <p className="text-label text-ink-300 break-words">
          Every email's unsubscribe link is built on{" "}
          <span className="num">{origin ?? "nothing — sends refuse until one is set"}</span>
          {stored ? " (set here)" : origin ? " (server default)" : ""}.
        </p>
      )}
      {error && (
        <p className="text-label text-fail break-words" role="alert">
          {error}
        </p>
      )}
      {note && (
        <p className="text-label text-ok" role="status">
          {note}
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
            <label className="text-label text-ink-300" htmlFor="email-origin">
              Origin (https; http on a loopback host)
            </label>
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
            <Button type="submit" size="sm" loading={pending} disabled={pending}>
              Save
            </Button>
            {stored && (
              <Button size="sm" disabled={pending} onClick={() => save(null)}>
                Clear
              </Button>
            )}
          </div>
        </form>
      )}
    </section>
  );
}
