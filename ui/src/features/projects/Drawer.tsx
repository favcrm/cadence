import { useEffect, useId, useMemo, useRef, useState } from "react";
import Button from "../../ui/Button";
import Md from "../../ui/Md";
import Select from "../../ui/Select";
import { IconCheck, IconClose } from "../../ui/icons";
import type { ResourceState } from "../../lib/cache";
import { api, ApiError, type AssignmentContext, type WriteResp } from "../../lib/api";
import { resources } from "../../lib/resources";
import { activityTimeMs } from "../../lib/fmt";
import { privateReadsAllowed, privateSessionGeneration } from "../../lib/privateSession";
import type { IssueDetail } from "../../lib/types";
import { useLocale } from "../../lib/locale";
import { STATUS_CHIP, PRIORITY_CHIP } from "../issues/chips";
import { acceptanceItems, laneState, peekSummary, timelineRows } from "../issues/model";
import AssignmentSummary from "../home/AssignmentSummary";
import { noDragReason } from "./Card";

/** Unsaved comment/status input the parent keeps per issue+identity across
 *  peek close/reopen. `status` is the pending selection — only an explicit
 *  Save commits it; the field is `null` once saved. */
export interface MyWorkDraft {
  comment: string;
  status: string | null;
  /** Shared submit guard survives closing/reopening the same issue mid-request. */
  pending?: { token: symbol; kind: "comment" | "status" };
}

/** The parent's read of whether this issue is still personally assigned.
 *  `checking` covers loading/stale/inFlight/failed — unknown is not a grant. */
export type MyWorkMembership = "checking" | "assigned" | "unassigned";

/**
 * Opt-in My work actions for the peek. Absent ⇒ the shipped read-only
 * peek. The parent owns `drafts` (a stable `useRef` map cleared on
 * `onPrivateSessionReset`); `draftKey` is `identityKey` + issue id.
 */
export interface MyWorkPeekProps {
  /** The verified `MyWorkPayload.owner` — "" while membership is `checking`. */
  owner: string;
  /** This issue's recorded handoff, or null when provenance is unavailable. */
  assignment: AssignmentContext | null;
  /** Verified session identity the drafts are keyed to. */
  identityKey: string;
  /** Parent-owned draft map — mutate in place, never replace. */
  drafts: Map<string, MyWorkDraft>;
  /** `myWork.draftKey` for this mounted issue (`identityKey` + id). */
  draftKey: string;
  membership: MyWorkMembership;
  onWrite: (resp: WriteResp, verb: string) => void;
  onError: (error: unknown, verb: string) => void;
  writeBlock: string | null;
  /** Notify other mounted views only when a shared submission settles/starts. */
  onDraftChange: () => void;
}

interface Props {
  id: string;
  detail: IssueDetail | null;
  readState?: ResourceState<IssueDetail> | null;
  onRetry?: () => void;
  /** Full issue page. Null when the project is unknown. */
  href: string | null;
  onClose: () => void;
  /** My work opt-in: owner context plus the bounded comment/status writes. */
  myWork?: MyWorkPeekProps;
}

const STATUSES = ["backlog", "ready", "doing", "review", "done", "dropped"];

interface Outcome {
  tone: "ok" | "err" | "warn" | "dim";
  text: string;
}

/** `t` is already the locale translate; kept out of the helper's signature. */
function writeWhy(
  personal: MyWorkPeekProps,
  detail: IssueDetail,
  readState: ResourceState<IssueDetail> | null | undefined,
  t: (s: string) => string,
): string | null {
  // Owner "" while checking means the parent has no verified owner yet — the
  // mismatch reason applies only once `assigned` confirms who should own it.
  if (personal.membership === "unassigned" ||
      (personal.membership === "assigned" && detail.owner !== personal.owner)) {
    return t("No longer assigned to you — quick actions are off.");
  }
  if (personal.writeBlock) return personal.writeBlock;
  if (personal.membership === "checking") {
    return t("Confirming this is still assigned to you — actions are paused.");
  }
  if (!readState) return t("Issue details unavailable — actions are off.");
  if (readState.inFlight || readState.status !== "ok") {
    return t("Details are not current — actions resume after a fresh read.");
  }
  return null;
}

/** A brief, modal preview. The full issue page owns triage and history. */
export default function Drawer({ id, detail, readState, onRetry, href, onClose, myWork }: Props) {
  const { t, formatRelativeTime } = useLocale();
  const dialogRef = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  const personal = myWork ?? null;

  /* Drafts survive close/reopen through the parent's map; one mounted Drawer
     is one issue (`key={openId}`), so mount state reads the stored draft. */
  const stored = personal?.drafts.get(personal.draftKey);
  const [draft, setDraft] = useState<Pick<MyWorkDraft, "comment" | "status">>(() => ({ comment: stored?.comment ?? "", status: stored?.status ?? null }));
  const saving = stored?.pending?.kind ?? null;
  useEffect(() => {
    const next = stored ?? { comment: "", status: null };
    setDraft((previous) => previous.comment === next.comment && previous.status === next.status ? previous : { comment: next.comment, status: next.status });
  }, [stored]);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const [requirementsOpen, setRequirementsOpen] = useState(false);
  const snapshotBody = detail?.body ?? "";
  const requirements = useMemo(() => {
    if (!requirementsOpen) return null;
    return snapshotBody.trim()
      ? <div className="issue-reader text-secondary text-ink-300"><Md text={snapshotBody} /></div>
      : <p className="issue-peek-note dim m-0">{t("No requirements recorded.")}</p>;
  }, [requirementsOpen, snapshotBody, t]);
  /* A discarded detail read means the server denied the record (401/403/404)
     — access loss, not a transport failure. The personal snapshot, drafts and
     the live link are cleared; the shared issue read keeps its own notice. */
  const denied = Boolean(personal) && readState?.discarded === true;
  useEffect(() => {
    if (!denied || !personal) return;
    if (personal.drafts.delete(personal.draftKey)) personal.onDraftChange();
    setDraft((previous) => previous.comment || previous.status ? { comment: "", status: null } : previous);
    setOutcome(null);
  }, [denied, personal]);
  // Mounting only controls DOM focus. Shared submission tokens, identity and
  // private-session generation govern asynchronous state across close/resume.
  const mounted = useRef(true);
  const ctxRef = useRef({ id, key: personal?.identityKey ?? null });
  ctxRef.current = { id, key: personal?.identityKey ?? null };
  const commentId = useId();
  const statusId = useId();
  const pendingFocus = useRef<{ kind: "comment" | "status"; key: string | null; generation: number } | null>(null);

  useEffect(() => {
    mounted.current = true;
    const dialog = dialogRef.current;
    const trigger = document.activeElement;
    const overflow = document.body.style.overflow;
    const scroll = window.scrollY;
    dialog?.showModal();
    document.body.style.overflow = "hidden";
    return () => {
      mounted.current = false;
      pendingFocus.current = null;
      dialog?.close();
      document.body.style.overflow = overflow;
      const queue = personal ? document.querySelector<HTMLElement>("[data-my-work]") : null;
      if (queue && queue.getClientRects().length > 0) {
        window.scrollTo(0, scroll);
        let target = trigger instanceof HTMLElement && trigger.isConnected ? trigger : null;
        if (!target) queue.querySelectorAll<HTMLElement>("[data-my-work-inspect]").forEach((element) => {
          if (target) return;
          const rect = element.getBoundingClientRect();
          if (rect.bottom >= 0 && rect.top < window.innerHeight) target = element;
        });
        target?.focus({ preventScroll: true });
      } else if (!personal && trigger instanceof HTMLElement && trigger.isConnected) {
        trigger.focus({ preventScroll: true });
      }
    };
  }, []);

  const updateDraft = (change: Partial<Pick<MyWorkDraft, "comment" | "status">>) => {
    const current: MyWorkDraft = personal?.drafts.get(personal.draftKey) ?? draft;
    if (current.pending) return;
    const next = { ...current, ...change };
    if (personal) {
      if (next.comment === "" && next.status === null) personal.drafts.delete(personal.draftKey);
      else personal.drafts.set(personal.draftKey, next);
    }
    setDraft({ comment: next.comment, status: next.status });
  };

  /** Keep post-save focus through later SSE reads, until the user moves away. */
  const refocus = (kind: "comment" | "status") => {
    pendingFocus.current = { kind, key: ctxRef.current.key, generation: privateSessionGeneration() };
  };
  useEffect(() => {
    const request = pendingFocus.current;
    if (!request) return;
    if (!mounted.current || !privateReadsAllowed() || request.key !== ctxRef.current.key ||
        request.generation !== privateSessionGeneration() || !dialogRef.current?.open) {
      pendingFocus.current = null;
      return;
    }
    const issueRead = resources.issue(id).get();
    const personalRead = resources.myWork.get();
    if (saving !== null || issueRead.inFlight || personalRead.inFlight) return;
    if (issueRead.status !== "ok" || personalRead.status !== "ok" || personal?.membership === "unassigned") {
      pendingFocus.current = null;
      return;
    }
    if (readState?.inFlight || personal?.membership === "checking") return;
    const target = document.getElementById(request.kind === "comment" ? commentId : statusId);
    if (!(target instanceof HTMLTextAreaElement || target instanceof HTMLButtonElement) || target.disabled || !document.hasFocus()) {
      pendingFocus.current = null;
      return;
    }
    if (document.activeElement === target) return;
    const frame = requestAnimationFrame(() => {
      if (pendingFocus.current !== request) return;
      if (resources.issue(id).get().inFlight || resources.myWork.get().inFlight || target.disabled) return;
      if (mounted.current && privateReadsAllowed() && request.key === ctxRef.current.key &&
          request.generation === privateSessionGeneration() && target.isConnected &&
          dialogRef.current?.open && document.hasFocus()) target.focus({ preventScroll: true });
      else pendingFocus.current = null;
    });
    return () => cancelAnimationFrame(frame);
  });

  const releaseFocusOutside = (target: EventTarget) => {
    const request = pendingFocus.current;
    if (!request) return;
    const control = document.getElementById(request.kind === "comment" ? commentId : statusId);
    if (!(target instanceof Node) || !control?.contains(target)) pendingFocus.current = null;
  };

  /**
   * Bounded write: comment post or explicit status Save. The request payload
   * and the issue/rev/identity context are captured at submit; the completion
   * re-checks that context and never auto-replays after a failure or 409.
   */
  const send = async (kind: "comment" | "status") => {
    if (!personal || !detail || denied || personal.drafts.get(personal.draftKey)?.pending) return;
    if (personal.membership !== "assigned" || detail.owner !== personal.owner) return;
    const liveRead = resources.issue(id).get();
    const livePersonal = resources.myWork.get();
    if (!privateReadsAllowed() || !liveRead.data || liveRead.discarded || liveRead.inFlight ||
        liveRead.status !== "ok" || liveRead.data.id !== id || liveRead.data.rev !== detail.rev ||
        !livePersonal.data || livePersonal.inFlight || livePersonal.status !== "ok" ||
        livePersonal.data.owner !== personal.owner || !livePersonal.data.issues.some((row) => row.id === id)) return;
    if (writeWhy(personal, liveRead.data, liveRead, t) !== null) return;
    if (kind === "status" && noDragReason(liveRead.data) !== null) return;
    const body = draft.comment.trim();
    const next = draft.status;
    if (kind === "comment" && !body) return;
    if (kind === "status" && (!next || next === detail.status)) return;
    const ctx = {
      id: detail.id, rev: detail.rev, identityKey: personal.identityKey,
      generation: privateSessionGeneration(), token: Symbol("peek-write"), body, status: next,
    };
    const sameScope = () => privateSessionGeneration() === ctx.generation &&
      ctxRef.current.id === ctx.id && ctxRef.current.key === ctx.identityKey;
    personal.drafts.set(personal.draftKey, { ...draft, pending: { token: ctx.token, kind } });
    personal.onDraftChange();
    setOutcome(null);
    try {
      const resp = kind === "comment"
        ? await api.comment(ctx.id, ctx.body, ctx.rev)
        : await api.patch(ctx.id, { status: ctx.status ?? undefined }, ctx.rev);
      if (!sameScope()) return;
      if (!privateReadsAllowed()) throw new Error("Session verification required");
      const resource = resources.issue(ctx.id);
      const currentDetail = resource.get().data;
      if (currentDetail && currentDetail.rev !== ctx.rev && currentDetail.rev !== resp.issue.rev) {
        // A different read won while this request ran. Do not overwrite it
        // with an unorderable content hash; refresh without replaying a write.
        void resource.invalidate();
        void resources.myWork.invalidate();
      } else personal.onWrite(resp, `${ctx.id} ${kind}`);
      if (resources.issue(ctx.id).get().discarded) {
        personal.drafts.delete(personal.draftKey);
        return;
      }
      const current = personal.drafts.get(personal.draftKey);
      if (current?.pending?.token !== ctx.token) return;
      const kept = { ...current };
      if (kind === "comment" && ctx.body === current.comment.trim()) kept.comment = "";
      if (kind === "status" && ctx.status === current.status) kept.status = null;
      personal.drafts.set(personal.draftKey, kept);
      // React Activity retains this state while hidden. A genuinely unmounted
      // instance ignores its state update; the shared map still settles.
      setDraft({ comment: kept.comment, status: kept.status });
      setOutcome({ tone: "ok", text: kind === "comment" ? t("Comment added.") : t("Status saved") });
      if (mounted.current) refocus(kind);
    } catch (error) {
      if (!sameScope()) return;
      if (privateReadsAllowed()) {
        personal.onError(error, `${ctx.id} ${kind}`);
        if (error instanceof ApiError && error.status === 409) {
          const resource = resources.issue(ctx.id);
          await resource.invalidate();
          if (resource.get().inFlight) await resource.refresh();
        }
        if (error instanceof ApiError && (error.status === 401 || error.status === 403 || error.status === 404)) {
          // Write refusal is not read refusal. Only the resulting current
          // read/identity retirement may purge the private snapshot and draft.
          void resources.issue(ctx.id).invalidate();
          void resources.myWork.invalidate();
        }
      }
      if (!sameScope()) return;
      setOutcome({
        tone: "err",
        text: error instanceof ApiError && error.status === 409
          ? t("Not saved — details changed since this snapshot loaded. Retry once fresh details are in; your draft is kept.")
          : t("Could not confirm the save — draft kept. Try again when ready."),
      });
      if (mounted.current) refocus(kind);
    } finally {
      if (privateSessionGeneration() === ctx.generation) {
        const current = personal.drafts.get(personal.draftKey);
        if (current?.pending?.token === ctx.token) {
          const { pending: _pending, ...kept } = current;
          if (kept.comment === "" && kept.status === null) personal.drafts.delete(personal.draftKey);
          else personal.drafts.set(personal.draftKey, kept);
        }
        personal.onDraftChange();
      }
    }
  };

  const agents = detail?.agents ?? [];
  const lane = agents.length === 0 ? t("No lane yet") : agents.map((a) => a.alias).join(", ");
  const accept = !detail || detail.checks.total === 0 ? "none" : `${detail.checks.done}/${detail.checks.total} ${detail.checks.total === 1 ? t("check") : t("checks")}`;
  const failed = !detail && readState?.status === "failed";
  // Only a confirmed denial (discarded read) drops the Details link — a
  // transport failure like a 503 keeps the real, independently readable URL.
  const fullHref = personal && denied ? null : href;
  const items = useMemo(() => acceptanceItems(snapshotBody), [snapshotBody]);
  const checksDone = items.reduce((count, item) => count + Number(item.checked), 0);
  const activity = useMemo(() => detail ? timelineRows(detail, []).slice(0, 5) : [], [detail]);

  const gone = Boolean(
    personal &&
      detail &&
      (personal.membership === "unassigned" ||
        (personal.membership === "assigned" && detail.owner !== personal.owner)),
  );
  const why = personal && detail && !gone ? writeWhy(personal, detail, readState, t) : null;
  const statusWhy = why ?? (detail ? noDragReason(detail) : null);
  const canComment = Boolean(draft.comment.trim());
  const statusChanged = draft.status !== null && draft.status !== detail?.status;
  const draftKept = Boolean(draft.comment.trim()) || statusChanged;

  return (
    <dialog
      ref={dialogRef}
      className="issue-peek bg-ink-875 border-l border-ink-700 text-ink-200"
      aria-labelledby={titleId}
      data-peek={id}
      onFocusCapture={(event) => releaseFocusOutside(event.target)}
      onPointerDownCapture={(event) => releaseFocusOutside(event.target)}
      onCancel={(event) => { event.preventDefault(); onClose(); }}
      onClick={(event) => {
        if (event.target !== event.currentTarget) return;
        const rect = event.currentTarget.getBoundingClientRect();
        if (event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom) onClose();
      }}
    >
      <div className="flex items-center gap-2">
        <div className="kicker">{t("Quick peek")}</div>
        <Button variant="ghost" size="sm" className="ml-auto issue-peek-close" aria-label={t("Close preview")} onClick={onClose} icon={<IconClose />} />
      </div>
      <div className="issue-peek-body" aria-busy={readState?.inFlight || undefined}>
        <div className="min-w-0">
          <div className="kicker">
            <span className="num">{id}</span>
            {detail ? ` · ${detail.project}` : ""}
          </div>
          <h2 id={titleId} className="text-cardtitle font-semibold text-ink-100 mt-1">{detail?.title ?? t("Issue preview")}</h2>
        </div>
        {gone ? (
          <div role="status">
            <p className="issue-peek-note warn m-0">{t("This issue is no longer assigned to you — quick actions are off.")}</p>
            <p className="issue-peek-note dim m-0">{t("It has left My work. You can still open the full issue while project access allows.")}</p>
          </div>
        ) : detail ? (
          <>
            <div className="flex flex-wrap gap-1.5">
              <span className={`chip ${STATUS_CHIP[detail.status] ?? "bg-ink-800 text-ink-400"}`}>{t(detail.status)}</span>
              <span className={`chip ${PRIORITY_CHIP[detail.priority] ?? "bg-ink-800 text-ink-400"}`}>{detail.priority}</span>
              {detail.blocked && <span className="chip bg-warn/15 text-warn">{t("Blocked")}</span>}
              {(detail.parent || detail.component) && <span className="chip bg-ink-800 text-ink-400">{detail.parent ?? detail.component}</span>}
            </div>
            <p className="text-secondary text-ink-400 m-0">{peekSummary(detail.body)}</p>
            <dl className="grid grid-cols-[92px_minmax(0,1fr)] gap-x-2 gap-y-1.5">
              <dt className="slabel">{t("Lane")}</dt>
              <dd className="text-secondary text-ink-200 min-w-0 truncate" title={lane}>{lane}{agents.length > 0 ? ` · ${laneState(agents)}` : ""}</dd>
              <dt className="slabel">{t("Accept")}</dt>
              <dd className="text-secondary text-ink-200">{accept}</dd>
              <dt className="slabel">{t("Owner")}</dt>
              <dd className="text-secondary text-ink-200">
                {personal && detail.owner === personal.owner ? t("you") : (detail.owner ?? t("unassigned"))}
              </dd>
              {personal && (
                <>
                  <dt className="slabel">{t("Assignment")}</dt>
                  <dd className="text-secondary text-ink-200 min-w-0">
                    <AssignmentSummary assignment={personal.assignment} full />
                  </dd>
                </>
              )}
            </dl>
            {detail.blocked && detail.blocked_reason && (
              <p className="issue-peek-note warn m-0" role="status">
                <strong>{t("Blocked.")}</strong> {detail.blocked_reason}{" "}
                {personal && !why ? t("Comments stay open; kick-off stays on the full issue.") : ""}
              </p>
            )}
            {readState?.inFlight && (
              <p className="text-label text-ink-400 m-0" role="status">{t("Refreshing preview; showing saved details.")}</p>
            )}
            {readState?.status === "stale" && !readState.inFlight && (
              <div className="flex items-center gap-2 flex-wrap" role="status">
                <span className="text-label text-warn">{t("Showing saved details; the preview may be out of date.")}</span>
                {onRetry && <Button size="sm" onClick={onRetry}>{t("Retry")}</Button>}
              </div>
            )}
            {personal && (
              <section className="issue-peek-actions" aria-label={t("Quick actions")}>
                <span className="slabel">{t("Quick actions")}</span>
                <label className="slabel" htmlFor={commentId}>{t("Add a comment")}</label>
                <textarea
                  id={commentId}
                  className="field w-full"
                  rows={3}
                  value={draft.comment}
                  placeholder={t("Write a comment…")}
                  disabled={Boolean(why) || saving !== null}
                  onChange={(e) => updateDraft({ comment: e.target.value })}
                />
                <div className="issue-peek-row">
                  <Button
                    size="sm"
                    loading={saving === "comment"}
                    disabled={Boolean(why) || saving !== null || !canComment}
                    onClick={() => void send("comment")}
                  >
                    {t(saving === "comment" ? "Posting…" : "Add comment")}
                  </Button>
                  <label className="slabel ml-auto" htmlFor={statusId}>{t("Status")}</label>
                  <Select
                    id={statusId}
                    size="sm"
                    className="issue-peek-status"
                    value={draft.status ?? detail.status}
                    disabled={Boolean(statusWhy) || saving !== null}
                    title={statusWhy ?? undefined}
                    aria-label={t("Status")}
                    options={[
                      ...(STATUSES.includes(detail.status)
                        ? []
                        : [{ value: detail.status, label: t(detail.status) }]),
                      ...STATUSES.map((s) => ({ value: s, label: t(s) })),
                    ]}
                    onChange={(status) => updateDraft({ status })}
                  />
                  <Button
                    size="sm"
                    loading={saving === "status"}
                    disabled={Boolean(statusWhy) || saving !== null || !statusChanged}
                    onClick={() => void send("status")}
                  >
                    {t(saving === "status" ? "Saving…" : "Save")}
                  </Button>
                </div>
                {statusWhy && statusWhy !== why && <p className="issue-peek-note dim m-0">{t(statusWhy)}</p>}
                {why && <p className="issue-peek-note dim m-0">{why}</p>}
                {draftKept && saving === null && !outcome && <p className="issue-peek-note dim m-0" role="status">{t("Draft kept — nothing posts until you save.")}</p>}
              </section>
            )}
            {outcome && (
              <p className={`issue-peek-note ${outcome.tone} m-0`} role={outcome.tone === "err" ? "alert" : "status"}>{outcome.text}</p>
            )}
            {personal && (
              <>
                <details className="issue-peek-sec" onToggle={(event) => setRequirementsOpen(event.currentTarget.open)}>
                  <summary><span className="slabel">{t("Requirements")}</span></summary>
                  {requirements}
                </details>
                <details className="issue-peek-sec">
                  <summary><span className="slabel">{t("Checks")} · {checksDone}/{items.length}</span></summary>
                  {items.length === 0 ? (
                    <p className="issue-peek-note dim m-0">{t("No checks recorded.")}</p>
                  ) : (
                    <ul className="issue-peek-list">
                      {items.map((item, index) => (
                        <li key={index} className={`issue-peek-check${item.checked ? " done" : ""}`}>
                          {item.checked ? <IconCheck size={12} /> : <IconClose size={12} />}
                          <span>{item.text}</span>
                        </li>
                      ))}
                    </ul>
                  )}
                </details>
                <details className="issue-peek-sec">
                  <summary><span className="slabel">{t("Recent activity")}</span></summary>
                  {activity.length === 0 ? (
                    <p className="issue-peek-note dim m-0">{t("No activity yet.")}</p>
                  ) : (
                    <div className="grid gap-1.5">
                      {activity.map((row, index) => (
                        <div key={index} className="issue-peek-act">
                          <div>
                            <span className="who">{row.title}</span>
                            {row.at && activityTimeMs(row.at) > 0 && (
                              <span className="when"> · {formatRelativeTime(row.at)}</span>
                            )}
                          </div>
                          <div>{row.detail}</div>
                        </div>
                      ))}
                    </div>
                  )}
                </details>
              </>
            )}
          </>
        ) : failed ? (
          <div className="text-label text-fail" role="alert">
            {denied ? (
              <p className="m-0">{t("This issue is no longer available to you — private details and any draft were cleared.")}</p>
            ) : (
              <p className="m-0">{t("Could not load this preview")}{readState?.error ? `: ${readState.error}` : "."}</p>
            )}
            {onRetry && <Button className="mt-3" onClick={onRetry}>{t("Retry")}</Button>}
          </div>
        ) : (
          <p className="text-secondary text-ink-400 m-0" role="status">{t("Loading issue…")}</p>
        )}
      </div>
      <div className="issue-peek-foot">
        {fullHref ? <Button variant="primary" full href={fullHref}>{t("Open full issue")}</Button> : <Button full disabled title={personal && failed ? t("Issue unavailable") : t("Project unknown")}>{t("Open full issue")}</Button>}
      </div>
    </dialog>
  );
}
