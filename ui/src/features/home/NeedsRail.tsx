import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useWriteBlock } from "../auth/WriteGate";
import type { ResourceState } from "../../lib/cache";
import type { Agent, AgentsPayload, DocumentFollowUp, DocumentFollowUps, Overview } from "../../lib/types";
import { kindSpec, metaLine, routineReviewHref, stagedNeed, todoSplit, updateTitle, withStaged, type HomeNeed } from "./needs";
import StagedWriteDetail, { noticeText } from "../decisions/StagedWriteDetail";
import { ageText, failureReason, isUncertain, isolate, outcomeOf, reasonText, titleOf, type StagedWrite } from "../decisions/stagedWrite";
import { homeDecisions, useDecisions } from "../decisions/useDecisions";
import AgentUpdates from "./AgentUpdates";
import type { AgentUpdate } from "./agentUpdateModel";
import { useTranslate } from "../../lib/locale";
import { READ_ONLY_COPY } from "./AnswerForm";
import TodoCard from "./TodoCard";
import { decisionReviewHead, nextDecisionToken, type HomeDecisionOpen } from "../../lib/workItems";
import { beginTodoVisit, doneTodo, hideTodo, marksFor, useTodoLocal } from "./todoLocal";
import { resources } from "../../lib/resources";
import { routinesApi } from "../agents/routinesApi";
import type { HostedTemplateRail } from "../templates/HostedTemplateReview";
import Button from "../../ui/Button";
import Link from "../../ui/Link";

export { NeedMenu } from "./NeedMenu";

export type DocumentFollowUpState = DocumentFollowUps | { status: "loading" };

function DocumentFollowUpRows({
  rows,
  onOpen,
  recent = false,
}: {
  rows: readonly (DocumentFollowUp & { outcome?: "approved" | "delivered" })[];
  onOpen: () => void;
  recent?: boolean;
}) {
  const t = useTranslate();
  const heading = t(recent ? "Document activity" : "Document follow-ups");
  return (
    <section className="border-b border-ink-700" aria-label={heading}>
      <h3 className="px-3 pt-2 text-label font-medium text-ink-400">{heading}</h3>
      <ul className="todo-list">
        {rows.map((row) => {
          const href = `/wiki?file=${encodeURIComponent(row.id)}&version=${row.version}`;
          const version = `v${row.version}`;
          const meta = recent ? t(row.outcome === "delivered" ? "Delivered" : "Approved") : t("Approval requested");
          const action = t(recent ? "Open" : "Review");
          return (
            <li key={`${row.id}:${row.version}`} className="flex min-w-0 items-center justify-between gap-2 border-b border-ink-800 px-3 py-2.5">
              <div className="min-w-0">
                <span className="block break-words text-label font-medium text-ink-100">{row.title}</span>
                <span className="block break-words text-micro text-ink-400">
                  {row.project ? `${row.project} · ` : ""}{version} · {meta}
                </span>
              </div>
              <Link href={href} className={recent ? "lnk text-label shrink-0" : "btn btn-sm btn-secondary shrink-0"} onClick={onOpen} aria-label={`${action}: ${row.title}, ${version}`}>
                {action}
              </Link>
            </li>
          );
        })}
      </ul>
    </section>
  );
}

/**
 * The Home rail shows actions for the viewer in "To do" and informational
 * rows and recent work in "Updates". Private Home supplies only documents;
 * it does not mount Master activity. Desktop can collapse the rail to
 * its count; under ~1100px it is a
 * slide-over opened from the floating button. Reading items (plans,
 * ideas, merge decisions) open inside the one shared work-item detail,
 * mounted by App (CAD-1811); the rail hands it the reading queue as it
 * stood when the item opened. The count and the sidebar's Home badge
 * include pending items only.
 */
export default function NeedsRail({
  overview,
  documentFollowUps,
  readOnly,
  scopedHost = false,
  hostedRoster,
  onOpenIssue,
  overviewHref,
  permissionsHref,
  onAsk,
  onOpenDecision,
  collapsed,
  onToggleCollapse,
  onAskAgent,
  onSent,
  decisions: decisionsOn = false,
  templateRail,
  templateCount,
}: {
  /** Only Master Home supplies the existing operator overview. */
  overview?: ResourceState<Overview>;
  documentFollowUps?: DocumentFollowUpState;
  readOnly: boolean;
  scopedHost?: boolean;
  hostedRoster?: AgentsPayload | null;
  onOpenIssue?: (id: string) => void;
  overviewHref?: string;
  /** Settings → Master permissions, where the standing "Always" rules live. */
  permissionsHref?: string;
  onAsk?: (need: HomeNeed, lead?: string) => void;
  /** Open a reading item in the shared detail (App owns the one mount). */
  onOpenDecision?: (open: HomeDecisionOpen) => void;
  collapsed: boolean;
  onToggleCollapse: () => void;
  onAskAgent?: (agent: Agent, update?: AgentUpdate) => void;
  /** A "Fix it" reached Master: Home pins the chat to the sent message. */
  onSent?: () => void;
  /** CAD-2186: also read the owner's staged decisions (`/api/assist/actions`) and
   *  list them in To do. Off for a board with no such route or a non-owner. */
  decisions?: boolean;
  templateRail?: HostedTemplateRail;
  templateCount?: number;
}) {
  const [tab, setTab] = useState<"todo" | "updates">("todo");
  const block = useWriteBlock(readOnly);
  const t = useTranslate();
  const privateHome = overview === undefined;
  const overviewData = overview?.data;
  const pendingDocuments = documentFollowUps?.status === "ready" && !readOnly ? documentFollowUps.pending : [];
  const recentDocuments = documentFollowUps?.status === "ready" ? documentFollowUps.recent : [];
  const split = todoSplit(overviewData?.needs_me);
  const { updates, decided } = split;
  const staged = useDecisions(decisionsOn);
  const [decidedRoutineIds, setDecidedRoutineIds] = useState<Record<string, boolean>>({});
  const [decidingRoutineId, setDecidingRoutineId] = useState<string | null>(null);
  const [routineActionErr, setRoutineActionErr] = useState<string | null>(null);
  const waitingRoutines = (overviewData?.routine_needs ?? []).filter(
    (r) => !decidedRoutineIds[r.id],
  );
  const routineProposals = (overviewData?.routine_proposals ?? []).filter(
    (p) => !decidedRoutineIds[p.id],
  );
  const routineStepHolds = (overviewData?.routine_step_holds ?? []).filter(
    (run) => !decidedRoutineIds[run.id],
  );

  const decideProposal = async (id: string, decision: "approve" | "dismiss") => {
    if (decidingRoutineId || readOnly) return;
    setDecidingRoutineId(id);
    setRoutineActionErr(null);
    try {
      await routinesApi.proposalDecide(id, decision);
      setDecidedRoutineIds((prev) => ({ ...prev, [id]: true }));
      void resources.overview.refresh();
    } catch {
      setRoutineActionErr(t("Could not record routine proposal decision."));
    } finally {
      setDecidingRoutineId(null);
    }
  };

  const decideStepHold = async (id: string, decision: "approve" | "dismiss") => {
    if (decidingRoutineId || readOnly) return;
    setDecidingRoutineId(id);
    setRoutineActionErr(null);
    try {
      await routinesApi.runDecide(id, decision);
      setDecidedRoutineIds((prev) => ({ ...prev, [id]: true }));
      void resources.overview.refresh();
    } catch {
      setRoutineActionErr(t("Could not record step decision."));
    } finally {
      setDecidingRoutineId(null);
    }
  };
  const stagedCards = useMemo(() => staged.pending.map((w) => stagedNeed(w, t)), [staged.pending, t]);
  const todo = useMemo(() => withStaged(split.todo, stagedCards), [split.todo, stagedCards]);
  // The staged detail: the ids that were waiting when it opened, and where in them it is.
  const [review, setReview] = useState<{ queue: string[]; at: number } | null>(null);
  const [drawer, setDrawer] = useState(false);
  const local = useTodoLocal();
  // A new visit to Home: earlier decided and sent marks stop applying.
  useLayoutEffect(() => { if (!privateHome) beginTodoVisit(); }, [privateHome]);
  // The floating button that owns the slide-over's focus on the way out.
  const needBtn = useRef<HTMLButtonElement | null>(null);
  const drawerEl = useRef<HTMLElement | null>(null);
  const wasOpen = useRef(false);
  const reviewOpening = useRef(false);
  // The <1100px drawer is a dialog: it lands focus on itself, Escape
  // closes it, and closing hands focus back to the button that opened
  // it. An Ask hand-off is unaffected — the composer's own focus lands
  // after this one (child effects run before this parent's).
  useEffect(() => {
    if (!drawer) return;
    drawerEl.current?.focus();
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape") setDrawer(false);
    };
    addEventListener("keydown", onKey);
    return () => removeEventListener("keydown", onKey);
  }, [drawer]);
  useEffect(() => {
    // Opening a review closes the slide-over on purpose: the button behind
    // the scrim is not where focus belongs then.
    if (!drawer && wasOpen.current && !reviewOpening.current) needBtn.current?.focus();
    reviewOpening.current = false;
    wasOpen.current = drawer;
  }, [drawer]);
  // An issue opened from the slide-over leaves Home — drop the drawer so
  // a stale overlay never hangs over the navigation.
  const openIssue = (id: string) => {
    setDrawer(false);
    onOpenIssue?.(id);
  };
  const [showDecided, setShowDecided] = useState(false);
  const showTab = (next: "todo" | "updates") => {
    setTab(next);
    if (decisionsOn && next === "updates") homeDecisions.clearNotice();
  };
  // Updates on screen is Updates viewed: a failed write stops asking for attention.
  useEffect(() => {
    if (decisionsOn && tab === "updates" && !collapsed) homeDecisions.markFailedSeen();
  }, [decisionsOn, tab, collapsed, staged.decided]);
  const stagedOpener = useRef<Element | null>(null);
  /** The detail closed: the opener if it is still there, else the rail. */
  const closeStaged = () => {
    setReview(null);
    window.setTimeout(() => {
      const opener = stagedOpener.current;
      const back =
        opener instanceof HTMLElement && opener !== document.body && opener.isConnected && !opener.closest("[data-drawer]")
          ? opener
          : // Only the rail that is on screen: the column at >= 1100px, else the floating button.
            window.innerWidth >= 1100
            ? document.querySelector<HTMLElement>(".needsrail .todo-tabs > button")
            : needBtn.current;
      back?.focus({ preventScroll: true });
    }, 20);
  };

  const cards = todo.filter((n) => !marksFor(local, n).hidden);
  const isDone = (n: HomeNeed) => marksFor(local, n).doneText !== undefined;
  const count =
    cards.filter((n) => !isDone(n)).length +
    waitingRoutines.length +
    routineProposals.length +
    routineStepHolds.length +
    pendingDocuments.length + (templateCount ?? 0);
  const countKnown = (Boolean(overviewData) || (privateHome && (documentFollowUps?.status === "ready" || templateCount !== undefined))) &&
    (templateRail == null || templateCount !== undefined);
  const readers = cards.filter((n) => kindSpec(n).place === "drawer" && n.action.type !== "staged");
  const firstPermission = cards.findIndex((n) => !isDone(n) && n.kind === "master_permission");
  const ask = (need: HomeNeed, lead?: string) => {
    setDrawer(false);
    onAsk?.(need, lead);
  };
  /** One reading item, with the queue as it stood when the detail opened:
   *  "n of m" does not recount when the overview refreshes mid-run. */
  const decisionFor = (need: HomeNeed, queue: HomeNeed[]): HomeDecisionOpen => ({
    need,
    index: Math.max(queue.findIndex((n) => n.key === need.key), 0) + 1,
    total: Math.max(queue.length, 1),
    token: nextDecisionToken(),
    reviewHead: decisionReviewHead(need),
    next: (text) => {
      doneTodo(need, text);
      const at = queue.findIndex((n) => n.key === need.key);
      const following = queue.slice(at + 1).find((n) => !isDone(n));
      return following ? decisionFor(following, queue) : null;
    },
    ask: (lead) => ask(need, lead),
  });
  const reviewStaged = (queue: string[], at: number, opener: Element | null = document.activeElement) => {
    stagedOpener.current = opener;
    homeDecisions.clearNotice();
    reviewOpening.current = true;
    setDrawer(false);
    setReview({ queue, at });
  };
  // CAD-2364: a chip in the thread opens the same review card, over what is waiting now.
  // The chip is the opener: a click does not focus a button in every browser.
  const openStaged = useRef<(id: string, opener: HTMLElement) => void>(() => {});
  openStaged.current = (id: string, opener: HTMLElement) => {
    const queue = staged.pending.map((w) => w.id);
    const at = queue.indexOf(id);
    if (at >= 0) reviewStaged(queue, at, opener);
  };
  useEffect(() => {
    if (decisionsOn) return homeDecisions.onReviewRequest((id, opener) => openStaged.current(id, opener));
  }, [decisionsOn]);
  const openDecision = (key: string) => {
    const stagedCard = cards.find((n) => n.key === key && n.action.type === "staged");
    if (stagedCard) {
      // Not an issue-backed item: the staged detail is the rail's own, on the same shell.
      const queue = cards.flatMap((n) => (n.action.type === "staged" ? [n.action.id] : []));
      reviewStaged(queue, Math.max(queue.indexOf(stagedCard.action.type === "staged" ? stagedCard.action.id : ""), 0));
      return;
    }
    const need = readers.find((n) => n.key === key);
    if (!need) return;
    onOpenDecision?.(decisionFor(need, readers.filter((n) => n.key === key || !isDone(n))));
  };
  const todoBody = (
    <>
      {templateRail?.(() => {
        if (!drawer) return;
        reviewOpening.current = true;
        setDrawer(false);
      })}
      {block && <p className="todo-note">{t(READ_ONLY_COPY)}</p>}
      {staged.notice && review === null && (
        <p className="todo-note text-label text-ink-300 px-3 pt-2" role="status">
          <span>{noticeText(staged.notice, t)}</span>
          {staged.notice.outcome !== "still-waiting" && (
            <button type="button" className="lnk text-label" onClick={() => showTab("updates")}>
              {t("View in Updates")}
            </button>
          )}
        </p>
      )}
      {overview && !overviewData && overview.status === "failed" && (
        <p className="px-3 py-2.5 text-label text-fail break-words">{t("The list couldn't be loaded. It will try again.")}</p>
      )}
      {overview && !overviewData && overview.status !== "failed" && (
        <p className="px-3 py-2.5 text-label text-ink-500">{t("Reading what needs you…")}</p>
      )}
      {/* An unread or failed decision list is not an empty list. */}
      {overviewData && cards.length === 0 && decisionsOn && (staged.phase === "idle" || staged.phase === "loading") && (
        <p className="px-3 py-2.5 text-label text-ink-500">{t("Reading what needs you…")}</p>
      )}
      {overviewData && cards.length === 0 && decisionsOn && staged.phase === "error" && (
        <p className="px-3 py-2.5 text-label text-fail" role="alert">{t("The list couldn't be loaded. It will try again.")}</p>
      )}
      {countKnown && count === 0 && cards.length === 0 && documentFollowUps?.status !== "unavailable" && documentFollowUps?.status !== "loading" && !(decisionsOn && (staged.phase === "idle" || staged.phase === "loading" || staged.phase === "error")) && (
        <p className="px-3 py-3 text-label text-ink-400">{t(privateHome ? "Nothing needs your review." : "Nothing needs you. The team is working.")}</p>
      )}
      {documentFollowUps?.status === "loading" && (
        <p className="px-3 py-2.5 text-label text-ink-500">{t("Reading document follow-ups…")}</p>
      )}
      {documentFollowUps?.status === "unavailable" && (
        <p className="px-3 py-2.5 text-label text-warn">
          {t("Document follow-ups could not be loaded.")}{" "}
          <button type="button" className="lnk" onClick={() => void resources.overview.refresh()}>{t("Retry")}</button>
        </p>
      )}
      {pendingDocuments.length > 0 && <DocumentFollowUpRows rows={pendingDocuments} onOpen={() => setDrawer(false)} />}
      {routineActionErr && (
        <p className="px-3 py-2 text-micro text-fail break-words" role="alert">
          {routineActionErr}
        </p>
      )}
      {(waitingRoutines.length > 0 || routineProposals.length > 0 || routineStepHolds.length > 0) && (
        <ul className="todo-list" data-routine-needs>
          {routineStepHolds.map((run) => {
            const href = routineReviewHref(run.agent_alias, hostedRoster);
            const waitingStep =
              (run.checklist_state ?? run.checklist ?? []).find(
                (s) => s.state === "waiting_approval",
              )?.label ?? t("Ask for approval first");
            return (
              <li
                key={run.id}
                className="px-3 py-2.5 border-b border-ink-800 flex flex-wrap items-center justify-between gap-2 min-w-0"
              >
                <div className="min-w-0 flex-1">
                  <span className="block text-label font-medium text-ink-100 truncate">
                    {run.routine_name ?? run.routine_id}
                  </span>
                  <span className="block text-micro text-warn truncate">
                    {waitingStep}
                  </span>
                </div>
                <div className="flex flex-wrap items-center gap-1.5 shrink-0">
                  {!readOnly && (
                    <>
                      <Button
                        size="sm"
                        variant="primary"
                        loading={decidingRoutineId === run.id}
                        disabled={decidingRoutineId !== null}
                        onClick={() => void decideStepHold(run.id, "approve")}
                      >
                        {t("Approve & send")}
                      </Button>
                      <Button
                        size="sm"
                        variant="secondary"
                        disabled={decidingRoutineId !== null}
                        onClick={() => void decideStepHold(run.id, "dismiss")}
                      >
                        {t("Dismiss")}
                      </Button>
                    </>
                  )}
                  {href && <Link
                    href={href}
                    className="btn btn-sm btn-secondary"
                    onClick={() => setDrawer(false)}
                  >
                    {t("Review")}
                  </Link>}
                </div>
              </li>
            );
          })}
          {routineProposals.map((p) => {
            const explicitAlias = typeof p.payload?.agent_alias === "string" ? p.payload.agent_alias : null;
            const wikiPath = typeof p.payload?.path === "string" ? p.payload.path : null;
            const href =
              p.kind === "wiki_propose" && wikiPath
                ? `/wiki/${wikiPath.replace(/^wiki:/, "")}`
                : routineReviewHref(explicitAlias, hostedRoster);
            return (
              <li key={p.id} className="px-3 py-2.5 border-b border-ink-800 flex flex-wrap items-center justify-between gap-2 min-w-0">
                <div className="min-w-0 flex-1">
                  <span className="block text-label font-medium text-ink-100 truncate">
                    {p.title}
                  </span>
                  <span className="block text-micro text-ink-400 truncate">
                    {p.kind === "wiki_propose" ? t("Playbook draft") : t("Routine draft")}
                  </span>
                </div>
                <div className="flex flex-wrap items-center gap-1.5 shrink-0">
                  {!readOnly && (
                    <>
                      <Button
                        size="sm"
                        variant="primary"
                        loading={decidingRoutineId === p.id}
                        disabled={decidingRoutineId !== null}
                        onClick={() => void decideProposal(p.id, "approve")}
                      >
                        {t("Approve")}
                      </Button>
                      <Button
                        size="sm"
                        variant="secondary"
                        disabled={decidingRoutineId !== null}
                        onClick={() => void decideProposal(p.id, "dismiss")}
                      >
                        {t("Dismiss")}
                      </Button>
                    </>
                  )}
                  {href && <Link
                    href={href}
                    className="btn btn-sm btn-secondary"
                    onClick={() => setDrawer(false)}
                  >
                    {t("Review")}
                  </Link>}
                </div>
              </li>
            );
          })}
          {waitingRoutines.map((r) => {
            const href = routineReviewHref(r.agent_alias, hostedRoster);
            return (
              <li key={r.id} className="px-3 py-2.5 border-b border-ink-800 flex items-center justify-between gap-2 min-w-0">
                <div className="min-w-0">
                  <span className="block text-label font-medium text-ink-100 truncate">
                    {r.name}
                  </span>
                  <span className="block text-micro text-ink-400 truncate">
                    {r.status === "repoint_required"
                      ? t("Choose thread")
                      : r.approval_drift && (r.approved_rev !== null || r.approved_sha256 !== null)
                        ? t("Playbook or script updated — approve to run")
                        : t("Approve routine before first run")}
                  </span>
                </div>
                {href && <Link
                  href={href}
                  className="btn btn-sm btn-secondary shrink-0"
                  onClick={() => setDrawer(false)}
                >
                  {t("Review")}
                </Link>}
              </li>
            );
          })}
        </ul>
      )}
      <ul className="todo-list">
        {cards.map((n, i) => (
          <TodoCard
            key={n.key}
            need={n}
            readOnly={readOnly}
            scopedHost={scopedHost}
            defaultOpen={i === firstPermission}
            doneText={marksFor(local, n).doneText}
            onReview={openDecision}
            onOpenIssue={openIssue}
            onAsk={(need) => ask(need)}
            onHide={hideTodo}
            onDone={doneTodo}
            onSent={() => {
              setDrawer(false);
              onSent?.();
            }}
          />
        ))}
      </ul>
      {cards.length > 0 && <p className="todo-hint">{t("Tap a card to see more. “Fix it” hands the problem to Master.")}</p>}
      {decided.length > 0 && (
        <section className="todo-decided" data-todo-decided>
          <button type="button" className="todo-decided-h" aria-expanded={showDecided} onClick={() => setShowDecided((s) => !s)}>
            <span className="steps-caret num" aria-hidden>
              ›
            </span>
            {t("Recently decided")} ({decided.length})
          </button>
          {showDecided && (
            <>
              <ul>
                {decided.map((n) => (
                  <li key={n.key}>
                    <span className="todo-decided-t">{n.why ?? n.shortTitle ?? kindSpec(n).title(n, { issueTitle: null })}</span>
                    <span className="chip bg-ink-800 text-ink-300 shrink-0">
                      {n.action.type === "permission" ? n.action.decisionLabel || n.action.status : "decided"}
                    </span>
                  </li>
                ))}
              </ul>
              {permissionsHref && <Link href={permissionsHref} className="lnk text-label" onClick={() => setDrawer(false)}>
                {t("Standing rules are in Settings")}
              </Link>}
            </>
          )}
        </section>
      )}
    </>
  );

  const updatesBody = (
    <>
      {recentDocuments.length > 0 && <DocumentFollowUpRows rows={recentDocuments} onOpen={() => setDrawer(false)} recent />}
      {privateHome && recentDocuments.length === 0 && (
        <p className="px-3 py-2.5 text-label text-ink-400">{t(documentFollowUps?.status === "loading" ? "Reading document follow-ups…" : documentFollowUps?.status === "unavailable" ? "Document follow-ups could not be loaded." : "No recent document activity.")}</p>
      )}
      {staged.decided.length > 0 && (
        <ul className="todo-fyi" data-todo-decisions>
          {staged.decided.slice(0, 8).map((w) => (
            <DecidedLine key={w.id} write={w} />
          ))}
        </ul>
      )}
      {updates.length > 0 && (
        <ul className="todo-fyi" data-todo-updates>
          {updates.map((n) => (
            <li key={n.key}>
              <span>{updateTitle(n)}</span>
              <span className="text-ink-500">
                {/* An Updates row stays information; the subject id is a
                    plain link to the issue it names, never an action. */}
                {n.issue !== null && (
                  <button type="button" className="lnk num" onClick={() => openIssue(n.issue!)}>
                    {n.issue}
                  </button>
                )}
                {n.issue !== null && " · "}
                {metaLine(n)}
              </span>
            </li>
          ))}
        </ul>
      )}
      {onAskAgent && <AgentUpdates
        onAsk={(agent, update) => {
          setDrawer(false);
          onAskAgent(agent, update);
        }}
        onOpenIssue={openIssue}
      />}
    </>
  );

  const tabs = (drawerMode: boolean) => (
    <div className="todo-tabs" role="group" aria-label={t("Home rail")}>
      <button aria-pressed={tab === "todo"} onClick={() => showTab("todo")}>
        {t("To do")} <span className={`todo-count ${count ? "on" : ""}`}>{countKnown ? count : "…"}</span>
      </button>
      <button aria-pressed={tab === "updates"} onClick={() => showTab("updates")}>
        {t("Updates")}
        {staged.failedUnseen && tab !== "updates" && (
          <span className="dx-dot" role="img" aria-label={t("A write was approved but did not apply")} />
        )}
      </button>
      <span className="todo-tools">
        {overviewHref && <Link href={overviewHref} className="lnk text-label" onClick={() => setDrawer(false)}>
          {t("Team overview")}
        </Link>}
        {drawerMode ? (
          <button type="button" className="lnk text-label" aria-label={t("close the rail")} onClick={() => setDrawer(false)}>
            ✕
          </button>
        ) : (
          <button
            type="button"
            className="lnk text-label"
            aria-label={collapsed ? "expand the rail" : "collapse the rail"}
            onClick={onToggleCollapse}
          >
            {collapsed ? "»" : "«"}
          </button>
        )}
      </span>
    </div>
  );

  const panelBody = <div>{tab === "todo" ? todoBody : updatesBody}</div>;

  return (
    <>
      {/* ≥1100px: the detached column — sticky in the grid, scrolling on
          its own; collapsed it is the slim rail with the count. */}
      <div className="hidden rail:block h-full min-h-0">
        {collapsed ? (
          <button
            type="button"
            className="slimrail card"
            aria-label={t("to do — {count} waiting; expand the rail").replace("{count}", String(count))}
            onClick={onToggleCollapse}
          >
            <span className={`chip ${count ? "bg-warn/10 text-warn" : "bg-ok/15 text-ok"}`}>
              {countKnown ? count : "…"}
            </span>
            <span className="vert text-label text-ink-400">{t("To do")}</span>
            <span className="text-ink-500" aria-hidden>
              «
            </span>
          </button>
        ) : (
          <section className="card needsrail overflow-hidden" aria-label={t("to do")} data-collapsed="false">
            {tabs(false)}
            <div className="rail-scroll min-h-0">{panelBody}</div>
          </section>
        )}
      </div>

      {/* <1100px: a floating button opens the slide-over drawer. */}
      <button
        type="button"
        className="needbtn rail:hidden"
        ref={needBtn}
        onClick={() => setDrawer(true)}
        aria-label={t("to do — {count} waiting; open the list").replace("{count}", String(count))}
      >
        {t("To do")}
        <span className={`chip ${count ? "bg-warn/15 text-warn" : "bg-ok/15 text-ok"}`}>
          {countKnown ? count : "…"}
        </span>
      </button>
      {review !== null && (
        <StagedWriteDetail
          decisions={staged}
          store={homeDecisions}
          queue={review.queue}
          at={review.at}
          readOnly={readOnly}
          onAt={(at) => setReview((r) => (r === null ? r : { ...r, at }))}
          onClose={closeStaged}
        />
      )}
      {drawer && (
        <>
          <div className="needsscrim rail:hidden" onClick={() => setDrawer(false)} />
          <section
            className="needsdrawer rail:hidden"
            role="dialog"
            aria-label={t("to do")}
            aria-modal="true"
            tabIndex={-1}
            ref={drawerEl}
          >
            {tabs(true)}
            <div className="rail-scroll min-h-0 flex-1">{panelBody}</div>
          </section>
        </>
      )}

    </>
  );
}

/** One decided write in Updates: title, what happened, how long ago. A failed apply says why. */
function DecidedLine({ write }: { write: StagedWrite }) {
  const t = useTranslate();
  const [raw, setRaw] = useState(false);
  const outcome = outcomeOf(write);
  const when = ageText(write.decidedAt ?? write.createdAt, t);
  if (outcome === "failed") {
    return (
      <li data-fail>
        <span><bdi>{isolate(titleOf(write, t))}</bdi></span>
        <span className="text-ink-500">
          <span className="dx-bad">{t("Did not apply")}</span> · {when}
        </span>
        <span className="dx-why">
          {reasonText(failureReason(write), t)}
          <button type="button" className="lnk dx-rawbtn" aria-expanded={raw} onClick={() => setRaw((r) => !r)}>
            {raw ? t("Hide raw") : t("Show raw")}
          </button>
          {raw && (
            <pre className="rv-tech dx-raw" tabIndex={0}>
              {write.result?.code ? `${t("code")}: ${write.result.code}\n\n` : ""}
              {write.raw}
            </pre>
          )}
        </span>
      </li>
    );
  }
  const label =
    outcome === "unconfirmed"
      ? t("Result not confirmed")
      : outcome === "denied"
      ? t("Denied")
      : write.kind === "create_issue" && write.result?.ticket
        ? `${t("Created")} ${write.result.ticket}`
        : write.connected !== null
          ? t(write.connected.effect === "send" ? "Sent" : "Drafted")
          : t("Applied");
  return (
    <li>
      <span><bdi>{isolate(titleOf(write, t))}</bdi></span>
      <span className="text-ink-500">
        {label} · {when}
      </span>
      {outcome === "unconfirmed" && write.connected !== null && isUncertain(write) && (
        <span className="dx-why">{reasonText(failureReason(write), t)}</span>
      )}
    </li>
  );
}
