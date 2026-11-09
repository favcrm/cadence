import { useCallback, useEffect, useId, useRef, useState } from "react";
import { ApiError } from "../../lib/apiError";
import { useTranslate } from "../../lib/locale";
import { IconCheck, IconClose, IconLock, IconRefresh, IconWarning } from "../../ui/icons";
import { appExplorer } from "../workspace-apps/workspaceApps";
import { appLoadErrorCopy } from "./appErrors";
import {
  activeGrant,
  activeGrants,
  conversationOf,
  formatGrantedAt,
  grantBlocker,
  grantReceiptMatches,
  grantWriteRefusal,
  initials,
  memberAccessErrorCopy,
  memberName,
  orphanActiveGrants,
  reasonCopy,
  reasonCopyFor,
  revokeReceiptMatches,
  revokeWriteRefusal,
  rosterMember,
  MemberAccessUnreadable,
  type MemberAccess,
  type MemberAccessConversation,
  type MemberAccessGrant,
  type RevokeReceipt,
} from "./memberAccess";
import { useModal } from "./shared";
import "./explorer.css";

/**
 * CAD-2215: the operator's exact-conversation member access on the Manage
 * page. One grant is one named workspace member plus one exact app
 * conversation; there is no app-wide or context-wide grant, and this panel
 * never claims one.
 *
 * The daemon is the authority. This panel:
 *  - renders only what `GET …/member-access` returned, validating it first
 *    (`memberAccess.ts`): an unreadable answer is an error, not an empty list;
 *  - offers a grant only when the answer carries the verified live roster and
 *    an approved digest, and re-reads that state immediately before writing;
 *  - sends the exact `(member_sub, conversation_id, expected_digest)` triple
 *    and treats a lost or malformed receipt as unconfirmed, never success;
 *  - cannot mutate anything for a viewer that is not an operator — Manage
 *    already refuses non-operators, and every control here re-checks.
 */

type Phase =
  | { kind: "loading" }
  | { kind: "readonly" }
  | { kind: "blocked"; message: string }
  | { kind: "error"; message: string }
  | { kind: "ready"; data: MemberAccess };

type Dialog =
  | { kind: "grant"; conversationId: string; digest: string }
  | { kind: "revoke"; grant: MemberAccessGrant; label: string };

type Notice = {
  tone: "ok" | "info" | "warn";
  text: string;
  /** Interrupt immediately (refusals); otherwise polite. */
  alert?: boolean;
};

type Pending = { kind: "grant" | "revoke"; conversationId: string; memberSub: string } | null;

type Unconfirmed = { kind: "grant" | "revoke"; conversationId: string; memberSub: string };

export default function MemberAccessPanel({
  installId,
  operator,
}: {
  installId: string;
  /** `ManageApp`'s own operator gate: `viewer.operator === true && !readOnly`. */
  operator: boolean;
}) {
  const t = useTranslate();
  const [phase, setPhase] = useState<Phase>({ kind: "loading" });
  const [attempt, setAttempt] = useState(0);
  const [conversationId, setConversationId] = useState<string | null>(null);
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const [pending, setPending] = useState<Pending>(null);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [unconfirmed, setUnconfirmed] = useState<Unconfirmed | null>(null);
  const busy = useRef(false);
  const panelRef = useRef<HTMLElement>(null);
  const dialogOpen = useRef(false);
  const titleId = `${useId()}-member-access-title`;

  const retry = useCallback(() => setAttempt((n) => n + 1), []);

  useEffect(() => {
    if (!operator) {
      setPhase({ kind: "readonly" });
      return;
    }
    const controller = new AbortController();
    let live = true;
    setPhase({ kind: "loading" });
    setNotice(null);
    setDialog(null);
    setPending(null);
    setUnconfirmed(null);
    busy.current = false;
    void appExplorer.memberAccess(installId, controller.signal)
      .then((data) => {
        if (!live || controller.signal.aborted) return;
        if (data.install_id !== installId) {
          setPhase({ kind: "error", message: "The board answered about a different app. Nothing was changed." });
          return;
        }
        setPhase({ kind: "ready", data });
        setConversationId((prev) =>
          prev && data.conversations.some((row) => row.id === prev) ? prev : data.conversations[0]?.id ?? null,
        );
      })
      .catch((cause: unknown) => {
        if (!live || controller.signal.aborted) return;
        const denied = cause instanceof ApiError && (cause.status === 401 || cause.status === 403);
        const known = cause instanceof ApiError ? reasonCopyFor(cause.code) : null;
        setPhase(
          denied
            ? { kind: "blocked", message: "This session can't read member access. Nothing was changed." }
            : {
                kind: "error",
                message:
                  known !== null
                    ? t(known)
                    : appLoadErrorCopy(cause, "Couldn't read the member access list. Try again in a moment."),
              },
        );
      });
    return () => {
      live = false;
      controller.abort();
    };
  }, [installId, attempt, operator]);

  // Closing a dialog whose opener row is gone must not leave focus on <body>.
  useEffect(() => {
    if (dialog !== null) {
      dialogOpen.current = true;
      return;
    }
    if (!dialogOpen.current) return;
    dialogOpen.current = false;
    const active = document.activeElement;
    if (active === null || active === document.body) {
      panelRef.current?.querySelector<HTMLButtonElement>("[data-ma-add]")?.focus();
    }
  }, [dialog]);

  const adopt = useCallback((data: MemberAccess) => {
    setPhase({ kind: "ready", data });
    setConversationId((prev) =>
      prev && data.conversations.some((row) => row.id === prev) ? prev : data.conversations[0]?.id ?? null,
    );
  }, []);

  /** Post-write read: null when the list could not be read, so nothing is assumed. */
  const readBack = useCallback(async (): Promise<MemberAccess | null> => {
    try {
      const data = await appExplorer.memberAccess(installId);
      return data.install_id === installId ? data : null;
    } catch {
      return null;
    }
  }, [installId]);

  const canWrite = operator && phase.kind === "ready" && pending === null && unconfirmed === null;
  const data = phase.kind === "ready" ? phase.data : null;
  const blocker = data ? grantBlocker(data) : null;
  const orphans = data ? orphanActiveGrants(data) : [];
  const selected = data && conversationId ? conversationOf(data, conversationId) : null;
  const convLabel = (conversation: MemberAccessConversation | null): string =>
    conversation === null
      ? t("this conversation")
      : conversation.title?.trim() || (conversation.general ? t("General") : t("Conversation"));

  /* ------------------------------- grants -------------------------------- */

  const performGrant = async (memberSub: string) => {
    if (busy.current || phase.kind !== "ready" || dialog?.kind !== "grant") return;
    const { conversationId: conv, digest } = dialog;
    busy.current = true;
    setPending({ kind: "grant", conversationId: conv, memberSub });
    setNotice(null);
    const who =
      memberName(phase.data, memberSub) ??
      (phase.data.members === null ? t("the member on this grant") : t("the unavailable member"));
    try {
      // Authoritative re-read immediately before the write: the exact
      // conversation, the still-current roster entry and the pinned digest
      // must all still hold.
      let fresh: MemberAccess;
      try {
        fresh = await appExplorer.memberAccess(installId);
      } catch {
        setDialog(null);
        setNotice({
          tone: "warn",
          alert: true,
          text: t("The access list couldn't be re-read, so nothing was granted. Try again."),
        });
        return;
      }
      if (fresh.install_id !== installId) {
        setDialog(null);
        setNotice({ tone: "warn", alert: true, text: t("The board answered about a different app. Nothing was granted.") });
        return;
      }
      adopt(fresh);
      const refusal = grantWriteRefusal(fresh, { conversationId: conv, memberSub, digest });
      if (refusal !== null) {
        setDialog(null);
        setNotice({ tone: "warn", alert: true, text: t(refusal) });
        return;
      }

      let receipt: MemberAccessGrant | null = null;
      let receiptUnreadable = false;
      let failure: unknown = null;
      try {
        receipt = await appExplorer.grantMemberAccess(installId, {
          member_sub: memberSub,
          conversation_id: conv,
          expected_digest: digest,
        });
      } catch (cause) {
        if (cause instanceof MemberAccessUnreadable) receiptUnreadable = true;
        else failure = cause;
      }
      // The receipt must name THIS request before it counts as evidence.
      const bound = receipt !== null && grantReceiptMatches(receipt, { installId, conversationId: conv, memberSub, digest });
      const confirmed = await readBack();
      setDialog(null);
      if (confirmed === null) {
        setUnconfirmed({ kind: "grant", conversationId: conv, memberSub });
        setNotice({
          tone: "warn",
          text: t(
            "The write's outcome couldn't be confirmed, so nothing is assumed. Read the access list again before acting.",
          ),
        });
        return;
      }
      adopt(confirmed);
      const row = activeGrant(confirmed, conv, memberSub);
      const refused = failure instanceof ApiError && failure.status >= 400 && failure.status < 500;
      const where = convLabel(conversationOf(confirmed, conv));
      const unavailableRecorded = {
        tone: "warn" as const,
        alert: true,
        text: t(
          "{name} is recorded on {conversation}, but the grant is unavailable: {reason} Review the row and revoke it if it should not stay.",
        )
          .replace("{name}", () => who)
          .replace("{conversation}", () => where)
          .replace("{reason}", () => t(reasonCopy(row?.reason ?? ""))),
      };

      // A receipt for another target is never success, even if a concurrent
      // actor changed the list while the write was in flight.
      if (receipt !== null && !bound) {
        setUnconfirmed({ kind: "grant", conversationId: conv, memberSub });
        setNotice({
          tone: "warn",
          alert: true,
          text: t(
            "The board's receipt for this grant named a different target, so nothing is assumed. Read the access list again before acting.",
          ),
        });
        return;
      }
      if (bound) {
        if (row !== null && row.usable) {
          setNotice({
            tone: "ok",
            text: t("Grant recorded for {name} on {conversation}. Other conversations, projects, and jobs are unchanged.")
              .replace("{name}", () => who)
              .replace("{conversation}", () => where),
          });
          return;
        }
        if (row !== null) {
          setNotice(unavailableRecorded);
          return;
        }
        setUnconfirmed({ kind: "grant", conversationId: conv, memberSub });
        setNotice({
          tone: "warn",
          text: t(
            "The grant was recorded but the list doesn't show it. Nothing is assumed — read the list again before retrying.",
          ),
        });
        return;
      }
      if (receiptUnreadable) {
        if (row !== null && row.usable) {
          setNotice({
            tone: "warn",
            text: t("The receipt couldn't be read, but re-reading shows an active grant for {name} on {conversation}.")
              .replace("{name}", () => who)
              .replace("{conversation}", () => where),
          });
          return;
        }
        if (row !== null) {
          setNotice(unavailableRecorded);
          return;
        }
        setUnconfirmed({ kind: "grant", conversationId: conv, memberSub });
        setNotice({
          tone: "warn",
          text: t(
            "The receipt couldn't be read and the list doesn't show the grant. Nothing is assumed — read the list again before retrying.",
          ),
        });
        return;
      }
      if (refused) {
        setNotice(
          row !== null
            ? {
                tone: "warn",
                alert: true,
                text: t(
                  "The grant was refused. The list does show {name} on {conversation} — that may be a separate change; review it before relying on it.",
                )
                  .replace("{name}", () => who)
                  .replace("{conversation}", () => where),
              }
            : { tone: "warn", alert: true, text: t(memberAccessErrorCopy(failure, "The grant was refused. Nothing was granted.")) },
        );
        return;
      }
      if (row !== null) {
        setNotice({
          tone: "warn",
          alert: true,
          text: row.usable
            ? t("The answer was lost, but re-reading shows an active grant for {name} on {conversation}. Whether this request caused it is unconfirmed.")
                .replace("{name}", () => who)
                .replace("{conversation}", () => where)
            : t("The answer was lost, but re-reading shows a grant for {name} on {conversation} that is unavailable: {reason} Whether this request caused it is unconfirmed.")
                .replace("{name}", () => who)
                .replace("{conversation}", () => where)
                .replace("{reason}", () => t(reasonCopy(row.reason))),
        });
        return;
      }
      setUnconfirmed({ kind: "grant", conversationId: conv, memberSub });
      setNotice({
        tone: "warn",
        text: t("The answer was lost, and re-reading shows no grant for {name} on {conversation}. Whether this request reached the board is unconfirmed.")
          .replace("{name}", () => who)
          .replace("{conversation}", () => where),
      });
    } finally {
      busy.current = false;
      setPending(null);
    }
  };

  const performRevoke = async () => {
    if (busy.current || phase.kind !== "ready" || dialog?.kind !== "revoke") return;
    const { grant, label: where } = dialog;
    const conv = grant.conversation_id;
    const memberSub = grant.member_sub;
    busy.current = true;
    setPending({ kind: "revoke", conversationId: conv, memberSub });
    setNotice(null);
    const who =
      memberName(phase.data, memberSub) ??
      (phase.data.members === null ? t("the member on this grant") : t("the unavailable member"));
    try {
      // Revoke needs no roster and no listed conversation: the exact saved
      // active grant is what must still exist. That is the only way an
      // archived or detached conversation's grant can be withdrawn.
      let fresh: MemberAccess;
      try {
        fresh = await appExplorer.memberAccess(installId);
      } catch {
        setDialog(null);
        setNotice({
          tone: "warn",
          alert: true,
          text: t("The access list couldn't be re-read, so nothing was revoked. Try again."),
        });
        return;
      }
      if (fresh.install_id !== installId) {
        setDialog(null);
        setNotice({ tone: "warn", alert: true, text: t("The board answered about a different app. Nothing was revoked.") });
        return;
      }
      adopt(fresh);
      const refusal = revokeWriteRefusal(fresh, { conversationId: conv, memberSub });
      if (refusal !== null) {
        setDialog(null);
        setNotice({ tone: "warn", alert: true, text: t(refusal) });
        return;
      }

      let receipt: RevokeReceipt | null = null;
      let receiptUnreadable = false;
      let failure: unknown = null;
      try {
        receipt = await appExplorer.revokeMemberAccess(installId, { member_sub: memberSub, conversation_id: conv });
      } catch (cause) {
        if (cause instanceof MemberAccessUnreadable) receiptUnreadable = true;
        else failure = cause;
      }
      // The receipt must name THIS member and THIS conversation's tombstone.
      const bound = receipt !== null && revokeReceiptMatches(receipt, { installId, conversationId: conv, memberSub });
      const confirmed = await readBack();
      setDialog(null);
      if (confirmed === null) {
        setUnconfirmed({ kind: "revoke", conversationId: conv, memberSub });
        setNotice({
          tone: "warn",
          text: t(
            "The revoke's outcome couldn't be confirmed, so nothing is assumed. Read the access list again before acting.",
          ),
        });
        return;
      }
      adopt(confirmed);
      const stillThere = activeGrant(confirmed, conv, memberSub) !== null;
      const refused = failure instanceof ApiError && failure.status >= 400 && failure.status < 500;

      if (receipt !== null && !bound) {
        setUnconfirmed({ kind: "revoke", conversationId: conv, memberSub });
        setNotice({
          tone: "warn",
          alert: true,
          text: t(
            "The board's receipt for this revoke named a different target, so nothing is assumed. Read the access list again before acting.",
          ),
        });
        return;
      }
      if (bound) {
        if (!stillThere) {
          setNotice({
            tone: "info",
            text: t(
              "Revoked {name} from {conversation}. Their page stops showing it at the next poll, and nothing they wrote was deleted.",
            )
              .replace("{name}", () => who)
              .replace("{conversation}", () => where),
          });
          return;
        }
        setUnconfirmed({ kind: "revoke", conversationId: conv, memberSub });
        setNotice({
          tone: "warn",
          text: t(
            "The revoke was recorded but the list still shows that active grant. Nothing is assumed — read the list again before retrying.",
          ),
        });
        return;
      }
      if (receiptUnreadable) {
        if (!stillThere) {
          setNotice({
            tone: "warn",
            text: t("The receipt couldn't be read, but re-reading shows no active grant for {name} on {conversation}.")
              .replace("{name}", () => who)
              .replace("{conversation}", () => where),
          });
          return;
        }
        setUnconfirmed({ kind: "revoke", conversationId: conv, memberSub });
        setNotice({
          tone: "warn",
          text: t(
            "The receipt couldn't be read and the list still shows that active grant. Nothing is assumed — read the list again before retrying.",
          ),
        });
        return;
      }
      if (refused) {
        setNotice(
          stillThere
            ? { tone: "warn", alert: true, text: t(memberAccessErrorCopy(failure, "The revoke was refused. Nothing was changed.")) }
            : {
                tone: "warn",
                alert: true,
                text: t(
                  "The revoke was refused, but the list no longer shows an active grant for {name} on {conversation}. Review it before relying on it.",
                )
                  .replace("{name}", () => who)
                  .replace("{conversation}", () => where),
              },
        );
        return;
      }
      // Lost or 5xx answer: state what the re-read actually shows; the
      // request's own effect stays unconfirmed either way.
      if (stillThere) {
        setUnconfirmed({ kind: "revoke", conversationId: conv, memberSub });
        setNotice({
          tone: "warn",
          alert: true,
          text: t(
            "The answer was lost, and re-reading still shows an active grant for {name} on {conversation}. Whether this request reached the board is unconfirmed.",
          )
            .replace("{name}", () => who)
            .replace("{conversation}", () => where),
        });
        return;
      }
      setNotice({
        tone: "warn",
        text: t(
          "The answer was lost, but re-reading shows no active grant for {name} on {conversation}. Whether this request caused that is unconfirmed.",
        )
          .replace("{name}", () => who)
          .replace("{conversation}", () => where),
      });
    } finally {
      busy.current = false;
      setPending(null);
    }
  };

  /* ------------------------------- markup -------------------------------- */

  return (
    <section className="card ma-panel" aria-labelledby={titleId} ref={panelRef}>
      <div className="ma-head">
        <div>
          <p className="slabel ma-operator">
            <IconLock size={12} /> {t("Operator only")}
          </p>
          <h2 id={titleId}>{t("Member access")}</h2>
          <p className="ma-lead">
            {t("Each grant is one member and one conversation. Projects and private jobs are not affected.")}
          </p>
        </div>
        {operator && (
          <div className="ma-head-actions">
            <button
              type="button"
              className="btn btn-primary btn-sm"
              data-ma-add
              disabled={!canWrite || blocker !== null || selected === null}
              title={
                blocker !== null
                  ? t(reasonCopy(blocker))
                  : !canWrite
                    ? t("Wait for the access list to be checked")
                    : undefined
              }
              onClick={() => {
                if (!data || !selected || data.digest === null || !canWrite) return;
                setDialog({ kind: "grant", conversationId: selected.id, digest: data.digest });
              }}
            >
              {t("Add member…")}
            </button>
          </div>
        )}
      </div>

      {phase.kind === "readonly" && (
        <p className="ma-lead mt-2">{t("Only a workspace operator can view or change member access.")}</p>
      )}

      {notice && (
        <div className={`alert ${notice.tone} ma-notice`} role={notice.alert ? "alert" : "status"}>
          <IconWarning size={14} />
          <div>{notice.text}</div>
        </div>
      )}
      {unconfirmed && (
        <div className="alert warn ma-notice" role="status">
          <IconWarning size={14} />
          <div>
            <b>{t("Checking the outcome.")}</b>{" "}
            {t("The last write is unconfirmed. Read the access list again before changing anything else.")}{" "}
            <button type="button" className="btn btn-sm" onClick={retry}>
              {t("Read again")}
            </button>
          </div>
        </div>
      )}

      {phase.kind === "loading" && (
        <div className="ma-toolbar">
          <p className="ma-count" role="status">
            {t("Reading the member access list…")}
          </p>
        </div>
      )}

      {phase.kind === "blocked" && (
        <div className="ma-blocked-note" role="status">
          <IconWarning size={14} />
          <div>
            <b>{t("Member access is unavailable right now.")}</b>
            <ul>
              <li>{t(phase.message)}</li>
              <li>{t("Nothing was changed. Existing grants keep whatever the board last confirmed.")}</li>
            </ul>
            <button type="button" className="btn btn-sm" onClick={retry}>
              <IconRefresh size={12} /> {t("Retry the access check")}
            </button>
          </div>
        </div>
      )}

      {phase.kind === "error" && (
        <div className="ma-error" role="alert">
          <h4>
            <IconWarning size={14} /> {t("Couldn't read the member access list")}
          </h4>
          <p>{t(phase.message)}</p>
          <p>{t("Nothing was changed — an unreadable answer is not an empty list.")}</p>
          <button type="button" className="btn btn-sm" onClick={retry}>
            <IconRefresh size={12} /> {t("Try again")}
          </button>
        </div>
      )}

      {data && (
        <>
          <fieldset className="ma-target">
            <legend className="slabel">{t("Conversation")}</legend>
            <div className="ma-convs" role="radiogroup" aria-label={t("Conversation to manage")}>
              {data.conversations.map((conversation, index) => {
                const checked = conversation.id === conversationId;
                const count = activeGrants(data, conversation.id).length;
                return (
                  <button
                    key={conversation.id}
                    type="button"
                    role="radio"
                    aria-checked={checked}
                    tabIndex={checked ? 0 : -1}
                    className="ma-conv"
                    disabled={pending !== null}
                    onClick={() => {
                      if (pending !== null) return;
                      setConversationId(conversation.id);
                      setNotice(null);
                    }}
                    onKeyDown={(event) => {
                      if (pending !== null) return;
                      const total = data.conversations.length;
                      let next: number | null = null;
                      if (event.key === "ArrowRight" || event.key === "ArrowDown") next = (index + 1) % total;
                      else if (event.key === "ArrowLeft" || event.key === "ArrowUp") next = (index - 1 + total) % total;
                      else if (event.key === "Home") next = 0;
                      else if (event.key === "End") next = total - 1;
                      if (next === null) return;
                      event.preventDefault();
                      setConversationId(data.conversations[next].id);
                      const group = event.currentTarget.parentElement;
                      group?.querySelectorAll<HTMLButtonElement>("[role='radio']")[next]?.focus();
                    }}
                  >
                    <span className="ma-conv-name">{convLabel(conversation)}</span>
                    <span className="ma-conv-meta">
                      {(count === 1 ? t("{count} active grant") : t("{count} active grants")).replace(
                        "{count}",
                        String(count),
                      )}{" "}
                      ·{" "}
                      {conversation.context_id ? t("app context") : t("no context")}
                    </span>
                  </button>
                );
              })}
            </div>
          </fieldset>

          {blocker !== null && (
            <p className="ma-blocked-inline" role="status">
              {t(reasonCopy(blocker))}
            </p>
          )}

          <div className="ma-toolbar">
            <p className="ma-count" role="status">
              {selected === null
                ? t("No conversation to manage.")
                : activeGrants(data, selected.id).length === 0
                  ? t("No active grants for {conversation}.").replace("{conversation}", () => convLabel(selected))
                  : activeGrants(data, selected.id).length === 1
                    ? t("{count} active grant for {conversation}.")
                        .replace("{count}", "1")
                        .replace("{conversation}", () => convLabel(selected))
                    : t("{count} active grants for {conversation}.")
                        .replace("{count}", String(activeGrants(data, selected.id).length))
                        .replace("{conversation}", () => convLabel(selected))}
            </p>
          </div>

          {selected === null ? (
            <div className="ma-empty" role="status">
              <p className="ma-empty-title">{t("This app has no conversation yet.")}</p>
              <p>{t("Member access needs an app conversation to name.")}</p>
            </div>
          ) : activeGrants(data, selected.id).length === 0 ? (
            <div className="ma-empty" role="status">
              <p className="ma-empty-title">
                {t("No active grants for {conversation} yet.").replace("{conversation}", () => convLabel(selected))}
              </p>
              <p>{t("Only you, the operator, can open it.")}</p>
              {operator && (
                <button
                  type="button"
                  className="btn btn-primary btn-sm"
                  disabled={!canWrite || blocker !== null}
                  onClick={() => {
                    if (!data || !selected || data.digest === null || !canWrite) return;
                    setDialog({ kind: "grant", conversationId: selected.id, digest: data.digest });
                  }}
                >
                  {t("Add the first member…")}
                </button>
              )}
            </div>
          ) : (
            <ul className="ma-list" aria-label={t("Saved member grants for {conversation}").replace("{conversation}", () => convLabel(selected))}>
              {activeGrants(data, selected.id).map((grant) => (
                <GrantRow
                  key={`${grant.conversation_id}\u0000${grant.member_sub}`}
                  grant={grant}
                  name={memberName(data, grant.member_sub)}
                  grantedBy={memberName(data, grant.granted_by)}
                  conversationLabel={convLabel(selected)}
                  rosterKnown={data.members !== null}
                  operator={operator}
                  canRevoke={canWrite}
                  onRevoke={() => setDialog({ kind: "revoke", grant, label: convLabel(selected) })}
                />
              ))}
            </ul>
          )}

          {orphans.length > 0 && (
            <section className="ma-orphans" aria-labelledby={`${titleId}-orphans`}>
              <h3 id={`${titleId}-orphans`}>{t("Grants for unavailable conversations")}</h3>
              <p className="ma-lead">
                {t(
                  "These conversations are no longer listed for this app, but these grants are still recorded. Revoking one removes only that saved grant; nothing else changes.",
                )}
              </p>
              <ul className="ma-list" aria-label={t("Grants for unavailable conversations")}>
                {orphans.map((grant) => (
                  <GrantRow
                    key={`orphan-${grant.conversation_id}\u0000${grant.member_sub}`}
                    grant={grant}
                    name={memberName(data, grant.member_sub)}
                    grantedBy={memberName(data, grant.granted_by)}
                    conversationLabel={t("an unavailable conversation")}
                    rosterKnown={data.members !== null}
                    operator={operator}
                    canRevoke={canWrite}
                    onRevoke={() => setDialog({ kind: "revoke", grant, label: t("an unavailable conversation") })}
                  />
                ))}
              </ul>
            </section>
          )}

          <p className="ma-foot">
            {t(
              "Revoking removes this conversation from their page without deleting their messages or changing their projects. A grant works only while the member is signed in. Private projects stay unavailable in this build.",
            )}
          </p>

          <TargetDisclosure access={data} conversation={selected} dialog={false} />
        </>
      )}

      {dialog?.kind === "grant" && data && (
        <GrantDialog
          access={data}
          conversation={conversationOf(data, dialog.conversationId)}
          pending={pending !== null}
          onClose={() => setDialog(null)}
          onConfirm={(memberSub) => void performGrant(memberSub)}
        />
      )}
      {dialog?.kind === "revoke" && data && (
        <RevokeDialog
          access={data}
          grant={dialog.grant}
          label={dialog.label}
          pending={pending !== null}
          onClose={() => setDialog(null)}
          onConfirm={() => void performRevoke()}
        />
      )}
    </section>
  );
}

/* ------------------------------- one grant ------------------------------- */

function GrantRow({
  grant,
  name,
  grantedBy,
  conversationLabel,
  rosterKnown,
  operator,
  canRevoke,
  onRevoke,
}: {
  grant: MemberAccessGrant;
  name: string | null;
  grantedBy: string | null;
  /** The readable conversation label; exact ids stay in the disclosure. */
  conversationLabel: string;
  /** `members !== null`: the roster was proved, so an absent name is a fact. */
  rosterKnown: boolean;
  operator: boolean;
  canRevoke: boolean;
  onRevoke: () => void;
}) {
  const t = useTranslate();
  const when = formatGrantedAt(grant.granted_at);
  const label = name ?? (rosterKnown ? t("Member unavailable") : t("Member not shown"));
  const meta = when
    ? grantedBy
      ? t("Granted {date} by {name}").replace("{date}", () => when).replace("{name}", () => grantedBy)
      : t("Granted {date}").replace("{date}", () => when)
    : t("Granted earlier");
  return (
    <li className="ma-row">
      <span className="ma-avatar" aria-hidden="true">
        {initials(name)}
      </span>
      <span className="ma-row-main">
        <span className="ma-row-who">
          <b>{label}</b>
          {grant.usable ? (
            <span className="chip ok">{t("Grant recorded")}</span>
          ) : (
            <span className="chip warn" title={t(reasonCopy(grant.reason))}>
              {t("Unavailable")}
            </span>
          )}
        </span>
        <small className="ma-row-meta">{meta}</small>
        {name === null && (
          <small className="ma-row-meta">
            {rosterKnown
              ? t("The member's roster entry can't be read. Check the exact target before revoking.")
              : t("The workspace roster can't be read, so this grant's member isn't named here. The exact target shows who it is.")}
          </small>
        )}
      </span>
      {operator && (
        <span className="ma-row-actions">
          <button
            type="button"
            className="btn btn-sm btn-danger"
            disabled={!canRevoke}
            aria-label={t("Revoke {name}'s access to {conversation}")
              .replace("{name}", () => label)
              .replace("{conversation}", () => conversationLabel)}
            onClick={onRevoke}
          >
            {t("Revoke…")}
          </button>
        </span>
      )}
    </li>
  );
}

/* -------------------------------- dialogs -------------------------------- */

function GrantDialog({
  access,
  conversation,
  pending,
  onClose,
  onConfirm,
}: {
  access: MemberAccess;
  conversation: MemberAccessConversation | null;
  pending: boolean;
  onClose: () => void;
  onConfirm: (memberSub: string) => void;
}) {
  const t = useTranslate();
  const ref = useModal<HTMLDivElement>(true, onClose);
  const base = useId();
  const listId = `${base}-roster`;
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const [active, setActive] = useState<string | null>(null);

  const conversationLabel = conversation === null
    ? t("this conversation")
    : conversation.title?.trim() || (conversation.general ? t("General") : t("Conversation"));
  const roster = access.members ?? [];
  const granted = new Set(
    conversation === null ? [] : activeGrants(access, conversation.id).map((row) => row.member_sub),
  );
  const needle = query.trim().toLowerCase();
  const options = needle === ""
    ? roster
    : roster.filter(
        (row) => row.name.toLowerCase().includes(needle) || row.role.toLowerCase().includes(needle),
      );
  const grantable = options.filter((row) => !granted.has(row.subject_id));
  const activeInOptions =
    active !== null && options.some((row) => row.subject_id === active && !granted.has(row.subject_id))
      ? active
      : null;
  const activeId = activeInOptions ?? grantable[0]?.subject_id ?? null;
  const picked = selected === null ? null : rosterMember(access, selected);

  const choose = (subjectId: string) => {
    if (granted.has(subjectId)) return;
    setSelected(subjectId);
  };

  return (
    <div className="scrim fixed inset-0 bg-black/50 z-50 grid place-items-center p-4" role="presentation" onClick={onClose}>
      <div
        ref={ref}
        tabIndex={-1}
        className="modal card p-5 max-w-lg w-full ma-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby={`${base}-grant-h`}
        aria-describedby={`${base}-grant-d`}
        onClick={(event) => event.stopPropagation()}
      >
        <div className="ma-dialog-head">
          <h3 id={`${base}-grant-h`}>
            {t("Grant access to {conversation}").replace("{conversation}", () => conversationLabel)}
          </h3>
          <span className="badge">{t("This conversation only")}</span>
        </div>
        <p id={`${base}-grant-d`} className="ma-dialog-lead">
          {t(
            "They can read this conversation, and nothing else of this app. Their projects and private jobs are not affected.",
          )}
        </p>

        <label className="ma-field" htmlFor={`${base}-search`}>
          <span className="slabel">{t("Member")}</span>
        </label>
        <input
          id={`${base}-search`}
          className="field ma-search"
          type="text"
          role="combobox"
          autoComplete="off"
          spellCheck={false}
          aria-expanded="true"
          aria-controls={listId}
          aria-autocomplete="list"
          aria-activedescendant={activeId ? `${listId}-${encodeURIComponent(activeId)}` : undefined}
          aria-describedby={`${base}-search-hint`}
          placeholder={t("Search the current roster")}
          value={query}
          disabled={pending}
          onChange={(event) => {
            setQuery(event.target.value);
            setSelected(null);
          }}
          onKeyDown={(event) => {
            const pool = options.filter((row) => !granted.has(row.subject_id));
            if (event.key === "ArrowDown" || event.key === "ArrowUp") {
              event.preventDefault();
              if (pool.length === 0) return;
              const from = Math.max(0, pool.findIndex((row) => row.subject_id === activeId));
              const next = event.key === "ArrowDown" ? (from + 1) % pool.length : (from - 1 + pool.length) % pool.length;
              setActive(pool[next].subject_id);
              return;
            }
            if (event.key === "Home" || event.key === "End") {
              event.preventDefault();
              if (pool.length === 0) return;
              setActive((event.key === "Home" ? pool[0] : pool[pool.length - 1]).subject_id);
              return;
            }
            if (event.key === "Enter") {
              event.preventDefault();
              const row = activeId === null ? null : options.find((item) => item.subject_id === activeId) ?? null;
              if (row !== null && !granted.has(row.subject_id)) choose(row.subject_id);
            }
          }}
        />
        <p className="ma-hint" id={`${base}-search-hint`}>
          {t("Arrow keys move through the list; Enter picks the active row. Their own sign-in is checked when they read, not now.")}
        </p>

        <ul className="ma-options" id={listId} role="listbox" aria-label={t("Current workspace roster")}>
          {options.length === 0 ? (
            <li className="ma-noresult" role="presentation">
              {t("No member matches “{query}”.").replace("{query}", () => query.trim())}
            </li>
          ) : (
            options.map((row) => {
              const already = granted.has(row.subject_id);
              const isActive = row.subject_id === activeId;
              return (
                <li
                  key={row.subject_id}
                  id={`${listId}-${encodeURIComponent(row.subject_id)}`}
                  role="option"
                  aria-selected={selected === row.subject_id}
                  aria-disabled={already || pending ? true : undefined}
                  data-active={isActive ? "true" : undefined}
                  className="ma-opt"
                  onMouseEnter={() => {
                    if (!already) setActive(row.subject_id);
                  }}
                  onMouseDown={(event) => event.preventDefault()}
                  onClick={() => {
                    if (pending) return;
                    choose(row.subject_id);
                  }}
                >
                  <span className="ma-avatar" aria-hidden="true">
                    {initials(row.name)}
                  </span>
                  <span className="ma-opt-copy">
                    <b>{row.name.trim() === "" ? t("Member unavailable") : row.name}</b>
                    <small>{t(row.role === "owner" ? "Owner" : "Member")}</small>
                  </span>
                  {already && <span className="ma-opt-note">{t("Already granted here")}</span>}
                </li>
              );
            })
          )}
        </ul>

        {picked && conversation !== null && (
          <div className="ma-eff">
            <div>
              <h4>
                <IconCheck size={12} /> {t("{name} will be able to read").replace("{name}", () => picked.name.split(/\s+/)[0] || picked.name)}
              </h4>
              <ul>
                <li>{t("{conversation} on their Apps page.").replace("{conversation}", () => conversationLabel)}</li>
                <li>{t("Its turn state and queue.")}</li>
              </ul>
            </div>
            <div>
              <h4>
                <IconLock size={12} /> {t("Not changed")}
              </h4>
              <ul>
                <li>{t("Other conversations of this app.")}</li>
                <li>{t("Project access.")}</li>
                <li>{t("Private jobs.")}</li>
              </ul>
            </div>
          </div>
        )}

        <TargetDisclosure
          access={access}
          conversation={conversation}
          memberSub={picked?.subject_id ?? null}
          dialog
        />

        <div className="ma-dialog-foot">
          <button type="button" className="btn" disabled={pending} onClick={onClose}>
            {t("Cancel")}
          </button>
          <button
            type="button"
            className="btn btn-primary"
            disabled={pending || picked === null}
            onClick={() => {
              if (picked !== null) onConfirm(picked.subject_id);
            }}
          >
            {pending ? (
              <>
                <span className="btn-spin" aria-hidden="true" /> {t("Granting…")}
              </>
            ) : (
              t("Grant access")
            )}
          </button>
        </div>
      </div>
    </div>
  );
}

function RevokeDialog({
  access,
  grant,
  label,
  pending,
  onClose,
  onConfirm,
}: {
  access: MemberAccess;
  grant: MemberAccessGrant;
  /** Readable conversation name, or "an unavailable conversation"; ids stay in the disclosure. */
  label: string;
  pending: boolean;
  onClose: () => void;
  onConfirm: () => void;
}) {
  const t = useTranslate();
  const ref = useModal<HTMLDivElement>(true, onClose);
  const base = useId();
  const conversation = conversationOf(access, grant.conversation_id);
  const name =
    memberName(access, grant.member_sub) ??
    (access.members === null ? t("Member not shown") : t("Member unavailable"));
  return (
    <div className="scrim fixed inset-0 bg-black/50 z-50 grid place-items-center p-4" role="presentation" onClick={onClose}>
      <div
        ref={ref}
        tabIndex={-1}
        className="modal card p-5 max-w-lg w-full ma-dialog"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby={`${base}-revoke-h`}
        aria-describedby={`${base}-revoke-d`}
        onClick={(event) => event.stopPropagation()}
      >
        <div className="ma-dialog-head">
          <h3 id={`${base}-revoke-h`}>
            {t("Revoke {name} from {conversation}?").replace("{name}", () => name).replace("{conversation}", () => label)}
          </h3>
          <span className="badge">{t("This conversation only")}</span>
        </div>
        <p id={`${base}-revoke-d`} className="ma-dialog-lead">
          {conversation === null
            ? t("The conversation is no longer listed for this app, but the grant is still recorded. Revoking removes only it; nothing they wrote is deleted.")
            : t("Their page stops showing this conversation at the next stream poll. Nothing they wrote is deleted.")}
        </p>
        <div className="ma-eff">
          <div>
            <h4>
              <IconClose size={12} /> {t("Stops")}
            </h4>
            <ul>
              <li>{t("{conversation} on their page.").replace("{conversation}", () => label)}</li>
              <li>{t("Its turn state and queue.")}</li>
            </ul>
          </div>
          <div>
            <h4>
              <IconCheck size={12} /> {t("Stays")}
            </h4>
            <ul>
              <li>{t("Other conversations of this app.")}</li>
              <li>{t("Project access and private jobs.")}</li>
            </ul>
          </div>
        </div>
        <TargetDisclosure
          access={access}
          conversation={conversation}
          memberSub={grant.member_sub}
          savedGrant={grant}
          dialog
        />
        <div className="ma-dialog-foot">
          <button type="button" className="btn" disabled={pending} onClick={onClose}>
            {t("Cancel")}
          </button>
          <button type="button" className="btn btn-danger" disabled={pending} onClick={onConfirm}>
            {pending ? (
              <>
                <span className="btn-spin" aria-hidden="true" /> {t("Revoking…")}
              </>
            ) : (
              t("Revoke access")
            )}
          </button>
        </div>
      </div>
    </div>
  );
}

/* --------------------------- evidence disclosure -------------------------- */

function TargetDisclosure({
  access,
  conversation,
  memberSub,
  savedGrant = null,
  dialog = false,
}: {
  access: MemberAccess;
  conversation: MemberAccessConversation | null;
  memberSub?: string | null;
  /** The saved grant row, so an unlisted conversation still shows its exact ids. */
  savedGrant?: MemberAccessGrant | null;
  /** Dialog copy repeats readable effects first; the panel owns the boundaries. */
  dialog?: boolean;
}) {
  const t = useTranslate();
  return (
    <details className={`ma-details${dialog ? " ma-details-dialog" : ""}`}>
      <summary>{dialog ? t("Exact target") : t("Exact target and boundaries")}</summary>
      <div className="ma-details-body">
        <dl className="ma-ident" aria-label={t("Exact grant target")}>
          <div>
            <dt>{t("Install")}</dt>
            <dd className="num">{access.install_id}</dd>
          </div>
          <div>
            <dt>{t("Context")}</dt>
            <dd className="num">{conversation?.context_id ?? savedGrant?.context_id ?? t("none")}</dd>
          </div>
          <div>
            <dt>{t("Conversation")}</dt>
            <dd>
              <b>{conversation === null ? t("Unavailable") : conversation.title?.trim() || (conversation.general ? t("General") : t("Conversation"))}</b>{" "}
              <span className="num">{conversation?.id ?? savedGrant?.conversation_id ?? t("unavailable")}</span>
            </dd>
          </div>
          <div>
            <dt>{savedGrant ? t("Grant's pinned package") : t("Approved package")}</dt>
            <dd className="num">{savedGrant?.install_digest ?? access.digest ?? t("unreadable")}</dd>
          </div>
          {memberSub ? (
            <div>
              <dt>{t("Member subject")}</dt>
              <dd className="num">{memberSub}</dd>
            </div>
          ) : null}
        </dl>
        {!dialog && (
          <ul className="ma-boundaries">
            <li>
              <b>{t("Projects")}</b>
              <span>{t("A grant never implies a project role.")}</span>
            </li>
            <li>
              <b>{t("Private jobs")}</b>
              <span>{t("Stay hidden, even on an issue the member can read.")}</span>
            </li>
            <li className="blocked">
              <b>{t("Private projects")}</b>
              <span>
                {t(
                  "Unavailable in this build until the OS isolation boundary is verified. This page cannot enable them.",
                )}
              </span>
              <span className="badge">{t("Unavailable")}</span>
            </li>
          </ul>
        )}
      </div>
    </details>
  );
}
