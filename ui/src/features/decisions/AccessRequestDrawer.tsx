import { useEffect, useState, type ReactNode } from "react";
import DrawerShell from "../app-shell/shared/DrawerShell";
import { READ_ONLY_COPY } from "../home/AnswerForm";
import { useTranslate } from "../../lib/locale";
import {
  ACCESS_REQUEST_NOTE_MAX,
  allowedGrantScopes,
  alreadyAccessText,
  durationLabel,
  scopeLabel,
  type AccessRequestDuration,
  type AccessRequestScope,
} from "../home/accessRequest";
import { ageText, isolate, type StagedWrite } from "./stagedWrite";
import type { Attempt } from "./useDecisions";

/**
 * AOS-816 (CAD-2885): Owner's slide-over drawer from Home -> To do for reviewing
 * a member's private `project` or `wiki` access request.
 *
 * Allows scoping down (e.g. `project_member` -> `wiki_read`, or `wiki_write` -> `wiki_read`)
 * and picking a duration (`ongoing`, `7d`, `30d`), never widening scope.
 */
const B = ({ children }: { children: ReactNode }) => (
  <bdi>{typeof children === "string" ? isolate(children) : children}</bdi>
);

const DURATIONS: readonly AccessRequestDuration[] = ["ongoing", "7d", "30d"];

export default function AccessRequestDrawer({
  write,
  attempt,
  busy,
  lost,
  uncertain,
  elsewhere,
  block,
  at,
  total,
  onApprove,
  onDecline,
  onCheck,
  onContinue,
  onSkip,
  onClose,
}: {
  write: StagedWrite;
  attempt: Attempt | undefined;
  busy: boolean;
  lost: boolean;
  uncertain: boolean;
  elsewhere: boolean;
  block: string | null;
  at: number;
  total: number;
  onApprove: (scope: AccessRequestScope, duration: AccessRequestDuration, clicks: number) => void;
  onDecline: (note: string, clicks: number) => void;
  onCheck: () => void;
  onContinue: () => void;
  onSkip: () => void;
  onClose: () => void;
}) {
  const t = useTranslate();
  const access = write.access!;
  const scopes = allowedGrantScopes(access.kind, access.requestedScope);
  const [scope, setScope] = useState<AccessRequestScope>(access.requestedScope);
  const [duration, setDuration] = useState<AccessRequestDuration>("ongoing");
  const [declining, setDeclining] = useState(false);
  const [note, setNote] = useState("");

  useEffect(() => {
    setScope(access.requestedScope);
    setDuration("ongoing");
    setDeclining(false);
    setNote("");
  }, [write.id, access.requestedScope]);

  const pending = write.status === "pending";
  const off = block !== null || busy || lost || uncertain || !pending || elsewhere;
  const approving = attempt?.phase === "busy" && attempt.verb === "approve";
  const decliningBusy = attempt?.phase === "busy" && attempt.verb === "deny";
  const warn = (text: string, role: "alert" | "status" = "alert") => (
    <p className="issue-peek-note warn m-0" role={role}>
      {text}
    </p>
  );

  const state = lost ? (
    <p className="issue-peek-note err m-0" role="alert">
      {t("Only a workspace owner can decide this access request.")}
    </p>
  ) : attempt?.phase === "elsewhere" && attempt.accessWinner ? (
    warn(alreadyAccessText(attempt.accessWinner, t))
  ) : elsewhere ? (
    warn(
      write.decidedBy
        ? t(write.status === "approved" ? "{name} already approved this." : "{name} already declined this.").replace(
            "{name}",
            () => write.decidedBy ?? "",
          )
        : t("This was already decided elsewhere. The list has been refreshed."),
    )
  ) : attempt?.phase === "refused" ? (
    warn(attempt.message)
  ) : uncertain ? (
    warn(
      t(
        "We could not confirm the result. It may already have gone through, so do not decide again; check first.",
      ),
    )
  ) : block !== null ? (
    warn(t(READ_ONLY_COPY), "status")
  ) : null;

  const foot = (
    <>
      <div className="crm-drawer-actions">
        {elsewhere ? (
          <button type="button" className="btn btn-sm btn-primary" onClick={onContinue}>
            {t("Continue")}
          </button>
        ) : uncertain ? (
          <button
            type="button"
            className="btn btn-sm btn-primary"
            aria-disabled={busy || undefined}
            onClick={onCheck}
          >
            {t("Check again")}
          </button>
        ) : declining ? (
          <>
            <button
              type="button"
              className="btn btn-sm btn-primary"
              data-access-decline
              aria-disabled={off || undefined}
              aria-busy={decliningBusy || undefined}
              onClick={(e) => onDecline(note, e.detail)}
            >
              {decliningBusy ? (
                <>
                  <span className="btn-spin" aria-hidden="true" />
                  {t("Declining…")}
                </>
              ) : (
                t("Decline request")
              )}
            </button>
            <button
              type="button"
              className="btn btn-sm"
              aria-disabled={busy || undefined}
              onClick={() => {
                if (!busy) {
                  setDeclining(false);
                  setNote("");
                }
              }}
            >
              {t("Back")}
            </button>
          </>
        ) : (
          <>
            <button
              type="button"
              className="btn btn-sm btn-primary"
              data-access-approve
              aria-disabled={off || undefined}
              aria-busy={approving || undefined}
              title={block !== null ? t(READ_ONLY_COPY) : undefined}
              onClick={(e) => onApprove(scope, duration, e.detail)}
            >
              {approving ? (
                <>
                  <span className="btn-spin" aria-hidden="true" />
                  {t("Granting access…")}
                </>
              ) : (
                t("Approve & grant")
              )}
            </button>
            <button
              type="button"
              className="btn btn-sm"
              data-access-decline-open
              aria-disabled={off || undefined}
              onClick={() => {
                if (!off) setDeclining(true);
              }}
            >
              {t("Decline…")}
            </button>
          </>
        )}
        <button
          type="button"
          className="btn btn-sm btn-ghost"
          aria-disabled={busy || undefined}
          onClick={onSkip}
        >
          {t("Not now")}
        </button>
      </div>
      <span className="crm-drawer-note" aria-live="polite">
        {t("{n} of {m}").replace("{n}", String(at + 1)).replace("{m}", String(total))}
      </span>
    </>
  );

  const narrowNote =
    access.kind === "project" && scope === "wiki_read" && access.requestedScope === "project_member"
      ? t("Grants read-only access to the project's wiki pages without exposing its tickets.")
      : access.kind === "wiki" && scope === "wiki_read" && access.requestedScope === "wiki_write"
        ? t("Narrowed to read-only access for this wiki path.")
        : null;

  return (
    <DrawerShell
      kind="staged"
      modal
      trapFocus
      scrim="board"
      size="md"
      preventClose={busy}
      label={t("Decision")}
      title={
        <B>
          {declining
            ? t("Decline {name}'s access request").replace("{name}", () => access.requester)
            : t("Grant {name} access?").replace("{name}", () => access.requester)}
        </B>
      }
      subtitle={
        <span>
          {t("Asked by")} <B>{access.requester}</B>
          {access.requesterHandle ? (
            <>
              {" "}
              (<B>{access.requesterHandle}</B>)
            </>
          ) : null}{" "}
          · {ageText(write.createdAt, t)}
        </span>
      }
      foot={foot}
      onClose={onClose}
    >
      <div className="wi-panel dx-panel" data-access-drawer>
        {state}
        {declining ? (
          <div className="dx-deny-box">
            <label className="rv-h" htmlFor="access-decline-note">
              {t("Note to {name} (optional)").replace("{name}", () => access.requester)}
            </label>
            <textarea
              id="access-decline-note"
              value={note}
              onChange={(e) => setNote(e.target.value)}
              maxLength={ACCESS_REQUEST_NOTE_MAX}
              rows={3}
              placeholder={t("Why are you declining? (optional)")}
            />
            <p className="rv-note m-0">
              {t("{name} sees that you declined, with your note. No access changes.").replace(
                "{name}",
                () => access.requester,
              )}
            </p>
          </div>
        ) : (
          <>
            <dl className="dx-kv-list">
              <div className="dx-kv-line">
                <dt>{t("Who")}</dt>
                <dd>
                  <B>{access.requester}</B>
                  {access.requesterHandle ? ` (${access.requesterHandle})` : ""} · {t("member")}
                </dd>
              </div>
              <div className="dx-kv-line">
                <dt>{t("Target")}</dt>
                <dd>
                  <B>{access.targetLabel}</B>
                </dd>
              </div>
              <div className="dx-kv-line">
                <dt>{t("Requested")}</dt>
                <dd>{scopeLabel(access.requestedScope, t)}</dd>
              </div>
            </dl>

            <section className="dx-sec" aria-label={t("Grant options")}>
              <h4 className="rv-h">{t("Grant options")}</h4>
              <div className="dx-kv-card">
                <div className="dx-kv-line">
                  <dt>
                    <label htmlFor="access-grant-scope">{t("Grant scope")}</label>
                  </dt>
                  <dd>
                    <select
                      id="access-grant-scope"
                      className="field w-full"
                      data-access-scope-select
                      value={scope}
                      disabled={off || scopes.length <= 1}
                      onChange={(e) => setScope(e.target.value as AccessRequestScope)}
                    >
                      {scopes.map((candidate) => (
                        <option key={candidate} value={candidate}>
                          {scopeLabel(candidate, t)}
                        </option>
                      ))}
                    </select>
                  </dd>
                </div>
                <div className="dx-kv-line">
                  <dt>
                    <label htmlFor="access-grant-duration">{t("Duration")}</label>
                  </dt>
                  <dd>
                    <select
                      id="access-grant-duration"
                      className="field w-full"
                      data-access-duration-select
                      value={duration}
                      disabled={off}
                      onChange={(e) => setDuration(e.target.value as AccessRequestDuration)}
                    >
                      {DURATIONS.map((d) => (
                        <option key={d} value={d}>
                          {durationLabel(d, t)}
                        </option>
                      ))}
                    </select>
                  </dd>
                </div>
                {narrowNote && (
                  <p className="rv-note m-0" data-access-narrow-note>
                    {narrowNote}
                  </p>
                )}
              </div>
            </section>

            {access.reason && (
              <section className="dx-sec" aria-label={t("Reason")}>
                <h4 className="rv-h">{t("Reason")}</h4>
                <blockquote className="dx-quote">
                  <B>{access.reason}</B>
                </blockquote>
              </section>
            )}
          </>
        )}
      </div>
    </DrawerShell>
  );
}
