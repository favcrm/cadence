import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useWriteBlock } from "../auth/WriteGate";
import type { ResourceState } from "../../lib/cache";
import type { Agent, AgentFeedItem, AgentsPayload, DocumentFollowUp, DocumentFollowUps, HostedLocalAssignmentTodo, Overview } from "../../lib/types";
import { ageWords, kindSpec, metaLine, stagedNeed, todoSplit, updateTitle, withStaged, type HomeNeed } from "./needs";
import StagedWriteDetail, { noticeText } from "../decisions/StagedWriteDetail";
import { ageText, failureReason, isUncertain, isolate, outcomeOf, reasonText, titleOf } from "../decisions/stagedWrite";
import { homeDecisions, useDecisions } from "../decisions/useDecisions";
import AgentUpdates, { type AgentFeedState } from "./AgentUpdates";
import type { AgentUpdate } from "./agentUpdateModel";
import { useTranslate } from "../../lib/locale";
import { READ_ONLY_COPY } from "./AnswerForm";
import TodoCard from "./TodoCard";
import { RailCard } from "./RailCard";
import RailTodoDrawer, { type RailTodoTarget } from "./RailTodoDrawer";
import RailFeedDrawer, { type RailFeedTarget } from "./RailFeedDrawer";
import {
  countFeedCategories,
  decidedWriteStatusLabel,
  FEED_CATEGORY_ORDER,
  feedCategoriesForDecidedWrite,
  feedCategoriesForInfoNeed,
  localAssignmentPresentation,
  presentTodoCategories,
  sortUnifiedTodos,
  todoCategoryForNeed,
  waitingRoutineReason,
  type FeedCategory,
  type FeedCategoryKey,
  type TodoCategory,
  type UnifiedTodoItem,
} from "./railModel";
import { decisionReviewHead, nextDecisionToken, type HomeDecisionOpen } from "../../lib/workItems";
import {
  beginTodoVisit,
  decideRoutine,
  doneTodo,
  hideTodo,
  isLocalAssignmentReviewed,
  localAssignmentFingerprint,
  marksFor,
  reconcileLocalAssignments,
  useTodoLocal,
} from "./todoLocal";
import { resources } from "../../lib/resources";
import type { HostedTemplateList, HostedTemplateRail } from "../templates/HostedTemplateReview";
import DrawerShell from "../app-shell/shared/DrawerShell";
import Link from "../../ui/Link";

export { NeedMenu } from "./NeedMenu";

export type DocumentFollowUpState = DocumentFollowUps | { status: "loading" };

const EMPTY_AGENT_FEED: AgentFeedState = {
  counts: { all: 0, action: 0, working: 0, uploaded: 0, completed: 0, external: 0 },
  liveCount: 0,
  hasAny: false,
};

function mergeFeedCounts(
  a: Record<FeedCategory, number>,
  b: Record<FeedCategory, number>,
): Record<FeedCategory, number> {
  return {
    all: a.all + b.all,
    action: a.action + b.action,
    working: a.working + b.working,
    uploaded: a.uploaded + b.uploaded,
    completed: a.completed + b.completed,
    external: a.external + b.external,
  };
}

/**
 * The Home rail shows actions for the viewer in "To do" (one flat priority-sorted
 * Card View with drawer-first actions) and read-only / talk-to-agent activity in
 * "Feed". Under ~1100px it is a slide-over opened from the floating button.
 */
export default function NeedsRail({
  overview,
  documentFollowUps,
  readOnly,
  canLocalDispatch = false,
  scopedHost = false,
  hostedRoster,
  onOpenIssue,
  onToast,
  permissionsHref,
  onAsk,
  onOpenDecision,
  onAskAgent,
  onAskFeedItem,
  onSent,
  decisions: decisionsOn = false,
  templateRail,
  templateCount,
  templateList,
  hasLiveAgents = false,
  streamDegraded = false,
  deepLinkActionId = null,
  onClearDeepLinkAction,
}: {
  /** Only Master Home supplies the existing operator overview. */
  overview?: ResourceState<Overview>;
  documentFollowUps?: DocumentFollowUpState;
  readOnly: boolean;
  canLocalDispatch?: boolean;
  scopedHost?: boolean;
  hostedRoster?: AgentsPayload | null;
  onOpenIssue?: (id: string) => void;
  onToast?: (msg: string) => void;
  /** Settings → Master permissions, where the standing "Always" rules live. */
  permissionsHref?: string;
  onAsk?: (need: HomeNeed, lead?: string) => void;
  /** Open a reading item in the shared detail (App owns the one mount). */
  onOpenDecision?: (open: HomeDecisionOpen) => void;
  onAskAgent?: (agent: Agent, update?: AgentUpdate) => void;
  onAskFeedItem?: (item: AgentFeedItem) => void;
  /** A "Fix it" reached Master: Home pins the chat to the sent message. */
  onSent?: () => void;
  /** CAD-2186: also read the owner's staged decisions (`/api/assist/actions`) and
   *  list them in To do. Off for a board with no such route or a non-owner. */
  decisions?: boolean;
  templateRail?: HostedTemplateRail;
  templateCount?: number;
  templateList?: HostedTemplateList;
  /** Live running agents known from parent workspace roster (drives green tab dot on both tabs). */
  hasLiveAgents?: boolean;
  /** True when the live event stream is degraded to 15s polling. */
  streamDegraded?: boolean;
  /** CAD-2825: staged action ID from `/?action=<actionId>` deep-link. */
  deepLinkActionId?: string | null;
  onClearDeepLinkAction?: () => void;
}) {
  const [tab, setTab] = useState<"todo" | "feed">("todo");
  const [todoFilter, setTodoFilter] = useState<TodoCategory>("all");
  const [feedFilter, setFeedFilter] = useState<FeedCategory>("all");
  const [todoTarget, setTodoTarget] = useState<RailTodoTarget | null>(null);
  const [feedTarget, setFeedTarget] = useState<RailFeedTarget | null>(null);
  const [agentFeedState, setAgentFeedState] = useState<AgentFeedState>(EMPTY_AGENT_FEED);

  const block = useWriteBlock(readOnly);
  const t = useTranslate();
  const privateHome = overview === undefined;
  const overviewData = overview?.data;
  const pendingDocuments = documentFollowUps?.status === "ready" && !readOnly ? documentFollowUps.pending : [];
  const recentDocuments = documentFollowUps?.status === "ready" ? documentFollowUps.recent : [];
  const split = todoSplit(overviewData?.needs_me);
  const { updates, decided } = split;
  const staged = useDecisions(decisionsOn);
  const local = useTodoLocal();
  // A new visit to Home: earlier decided and sent marks stop applying.
  useLayoutEffect(() => { if (!privateHome) beginTodoVisit(); }, [privateHome]);
  const waitingRoutines = (overviewData?.routine_needs ?? []).filter((r) => !local.decidedRoutines.has(r.id));
  const routineProposals = (overviewData?.routine_proposals ?? []).filter((p) => !local.decidedRoutines.has(p.id));
  const routineStepHolds = (overviewData?.routine_step_holds ?? []).filter((run) => !local.decidedRoutines.has(run.id));

  const stagedCards = useMemo(() => staged.pending.map((w) => stagedNeed(w, t)), [staged.pending, t]);
  const todo = useMemo(() => withStaged(split.todo, stagedCards), [split.todo, stagedCards]);

  // The staged detail: the ids that were waiting when it opened, and where in them it is.
  const [review, setReview] = useState<{ queue: string[]; at: number } | null>(null);
  const [drawerToken, setDrawerToken] = useState<number | null>(null);
  const drawerSeq = useRef(0);
  const drawer = drawerToken !== null;
  const openDrawer = () => {
    drawerSeq.current += 1;
    setDrawerToken(drawerSeq.current);
  };
  const closeDrawerImmediate = () => setDrawerToken(null);

  // The floating button that owns the slide-over's focus on the way out.
  const needBtn = useRef<HTMLButtonElement | null>(null);
  const wasOpen = useRef(false);
  const reviewOpening = useRef(false);
  const stagedOpener = useRef<Element | null>(null);
  const detailOpener = useRef<Element | null>(null);
  const desktopScrollRef = useRef<HTMLDivElement | null>(null);
  const drawerScrollRef = useRef<HTMLDivElement | null>(null);

  useLayoutEffect(() => {
    if (desktopScrollRef.current) desktopScrollRef.current.scrollTop = 0;
    if (drawerScrollRef.current) drawerScrollRef.current.scrollTop = 0;
  }, [tab, todoFilter, feedFilter, drawerToken]);

  useEffect(() => {
    if (!drawer && wasOpen.current && !reviewOpening.current) needBtn.current?.focus();
    reviewOpening.current = false;
    wasOpen.current = drawer;
  }, [drawer]);

  const restoreRailFocus = (opener: Element | null) => {
    window.setTimeout(() => {
      const back =
        opener instanceof HTMLElement &&
        opener !== document.body &&
        opener.isConnected &&
        !opener.closest("[data-drawer]")
          ? opener
          : // Only the rail that is on screen: the column at >= 1100px, else the floating button.
            window.innerWidth >= 1100
            ? document.querySelector<HTMLElement>(".needsrail .todo-tabs > button")
            : needBtn.current;
      back?.focus({ preventScroll: true });
    }, 20);
  };

  // An issue opened from the slide-over leaves Home — drop the drawer so
  // a stale overlay never hangs over the navigation.
  const openIssue = (id: string) => {
    closeDrawerImmediate();
    onOpenIssue?.(id);
  };
  const [showDecided, setShowDecided] = useState(false);
  const showTab = (next: "todo" | "feed") => {
    setTab(next);
    if (decisionsOn && next === "feed") homeDecisions.clearNotice();
  };

  // Feed on screen is Feed viewed: a failed write stops asking for attention.
  useEffect(() => {
    if (!decisionsOn || tab !== "feed") return;
    const markVisible = () => {
      if (drawer || window.innerWidth >= 1100) homeDecisions.markFailedSeen();
    };
    markVisible();
    window.addEventListener("resize", markVisible);
    return () => window.removeEventListener("resize", markVisible);
  }, [decisionsOn, tab, drawer, staged.decided]);

  const closeStaged = () => {
    setReview(null);
    restoreRailFocus(stagedOpener.current);
  };

  const openTodoDetail = (next: RailTodoTarget, opener: HTMLElement) => {
    detailOpener.current = opener;
    reviewOpening.current = true;
    closeDrawerImmediate();
    setTodoTarget(next);
  };

  const closeTodoDetail = (restoreFocus = true) => {
    setTodoTarget(null);
    if (restoreFocus !== false) restoreRailFocus(detailOpener.current);
  };

  const openFeedDetail = (next: RailFeedTarget, opener: HTMLElement | null) => {
    detailOpener.current = opener;
    reviewOpening.current = true;
    closeDrawerImmediate();
    setFeedTarget(next);
  };

  const closeFeedDetail = (restoreFocus = true) => {
    setFeedTarget(null);
    if (restoreFocus !== false) restoreRailFocus(detailOpener.current);
  };

  const cards = useMemo(() => todo.filter((n) => !marksFor(local, n).hidden), [todo, local]);
  const isDone = (n: HomeNeed) => marksFor(local, n).doneText !== undefined;

  const rawLocalAssignments = useMemo(
    () => overviewData?.local_assignment_todos ?? [],
    [overviewData?.local_assignment_todos],
  );
  const localAssignmentsSig = rawLocalAssignments
    .map((item) => `${item.issue_id}\t${localAssignmentFingerprint(item)}`)
    .join("\n");
  useEffect(() => {
    if (overviewData?.local_assignment_todos) reconcileLocalAssignments(overviewData.local_assignment_todos);
  }, [localAssignmentsSig]);

  const localAssignmentTodos = useMemo<HostedLocalAssignmentTodo[]>(
    () => rawLocalAssignments.filter((item) => !isLocalAssignmentReviewed(local, item)),
    [rawLocalAssignments, local],
  );

  useEffect(() => {
    if (todoTarget?.kind === "need" && !cards.some((n) => n.key === todoTarget.key)) {
      setTodoTarget(null);
    }
    if (
      todoTarget?.kind === "local_assignment" &&
      !localAssignmentTodos.some(
        (item) =>
          item.issue_id === todoTarget.item.issue_id &&
          localAssignmentFingerprint(item) === localAssignmentFingerprint(todoTarget.item),
      )
    ) {
      setTodoTarget(null);
    }
  }, [todoTarget, cards, localAssignmentTodos]);

  const pendingTemplates = useMemo(() => templateList?.pending ?? [], [templateList?.pending]);
  const approvedTemplates = useMemo(() => templateList?.approved ?? [], [templateList?.approved]);

  const unifiedTodos = useMemo<UnifiedTodoItem[]>(() => {
    const raw: UnifiedTodoItem[] = [
      ...cards.map((need): UnifiedTodoItem => ({
        kind: "need",
        key: `need:${need.key}`,
        category: todoCategoryForNeed(need),
        need,
      })),
      ...routineStepHolds.map((run): UnifiedTodoItem => ({
        kind: "routine_step_hold",
        key: `step:${run.id}`,
        category: "approvals",
        run,
      })),
      ...routineProposals.map((proposal): UnifiedTodoItem => ({
        kind: "routine_proposal",
        key: `proposal:${proposal.id}`,
        category: "approvals",
        proposal,
      })),
      ...waitingRoutines.map((routine): UnifiedTodoItem => ({
        kind: "waiting_routine",
        key: `routine:${routine.id}`,
        category: "reviews",
        routine,
      })),
      ...pendingDocuments.map((doc): UnifiedTodoItem => ({
        kind: "document",
        key: `doc:${doc.id}:${doc.version}`,
        category: "reviews",
        doc,
      })),
      ...pendingTemplates.map((template): UnifiedTodoItem => ({
        kind: "template",
        key: `tpl:${template.name}:${template.version}`,
        category: "reviews",
        template,
      })),
      ...localAssignmentTodos.map((item): UnifiedTodoItem => ({
        kind: "local_assignment",
        key: `hla:${item.issue_id}`,
        category: item.category,
        item,
      })),
    ];
    return sortUnifiedTodos(raw);
  }, [
    cards,
    routineStepHolds,
    routineProposals,
    waitingRoutines,
    pendingDocuments,
    pendingTemplates,
    localAssignmentTodos,
  ]);

  const todoCategories = useMemo(() => presentTodoCategories(unifiedTodos), [unifiedTodos]);
  useEffect(() => {
    if (todoFilter !== "all" && (todoCategories.length <= 1 || !todoCategories.some((c) => c.key === todoFilter))) {
      setTodoFilter("all");
    }
  }, [todoFilter, todoCategories]);

  const visibleTodos = useMemo(
    () => (todoFilter === "all" ? unifiedTodos : unifiedTodos.filter((item) => item.category === todoFilter)),
    [unifiedTodos, todoFilter],
  );

  const count =
    cards.filter((n) => !isDone(n)).length +
    waitingRoutines.length +
    routineProposals.length +
    routineStepHolds.length +
    pendingDocuments.length +
    localAssignmentTodos.length +
    (templateList ? templateList.pending.length : (templateCount ?? 0));
  const countKnown =
    (Boolean(overviewData) ||
      (privateHome &&
        (documentFollowUps?.status === "ready" ||
          templateCount !== undefined ||
          (templateList !== undefined && !templateList.loading) ||
          (decisionsOn && staged.phase === "ready")))) &&
    (templateList ? !templateList.loading && templateCount !== undefined : templateRail == null || templateCount !== undefined);

  const readers = useMemo(
    () =>
      unifiedTodos.flatMap((item) =>
        item.kind === "need" && kindSpec(item.need).place === "drawer" && item.need.action.type !== "staged"
          ? [item.need]
          : [],
      ),
    [unifiedTodos],
  );

  const ask = (need: HomeNeed, lead?: string) => {
    closeDrawerImmediate();
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
    closeDrawerImmediate();
    setReview({ queue, at });
  };

  // CAD-2364: a chip in the thread opens the same review card, over what is waiting now.
  const openStaged = useRef<(id: string, opener: HTMLElement) => void>(() => {});
  openStaged.current = (id: string, opener: HTMLElement) => {
    const queue = staged.pending.map((w) => w.id);
    const at = queue.indexOf(id);
    if (at >= 0) reviewStaged(queue, at, opener);
  };
  useEffect(() => {
    if (decisionsOn) return homeDecisions.onReviewRequest((id, opener) => openStaged.current(id, opener));
  }, [decisionsOn]);

  // CAD-2825: opening `/?action=<actionId>` waits for staged decisions to load,
  // auto-opens that exact action in StagedWriteDetail, or surfaces a non-destructive notice.
  const handledActionRef = useRef<string | null>(null);
  const [deepLinkNotice, setDeepLinkNotice] = useState<string | null>(null);
  useEffect(() => {
    if (!deepLinkActionId) {
      handledActionRef.current = null;
      return;
    }
    if (handledActionRef.current === deepLinkActionId) return;
    if (!decisionsOn || staged.phase === "unavailable") {
      handledActionRef.current = deepLinkActionId;
      const msg = t("Staged approvals are not available in this view.");
      setDeepLinkNotice(msg);
      onToast?.(msg);
      onClearDeepLinkAction?.();
      return;
    }
    if (staged.phase === "idle" || staged.phase === "loading") return;
    if (staged.phase === "error") {
      handledActionRef.current = deepLinkActionId;
      const msg = t("Could not load pending approvals to open that action. Try refreshing.");
      setDeepLinkNotice(msg);
      onToast?.(msg);
      onClearDeepLinkAction?.();
      return;
    }
    handledActionRef.current = deepLinkActionId;
    const queue = staged.pending.map((w) => w.id);
    const at = queue.indexOf(deepLinkActionId);
    if (at >= 0) {
      setDeepLinkNotice(null);
      setTab("todo");
      reviewStaged(queue, at, null);
      onClearDeepLinkAction?.();
      return;
    }
    const decidedMatch = staged.decided.find((w) => w.id === deepLinkActionId);
    if (decidedMatch) {
      const msg = t("That staged action has already been decided.");
      setDeepLinkNotice(msg);
      onToast?.(msg);
      openFeedDetail({ kind: "decided_write", write: decidedMatch }, null);
      onClearDeepLinkAction?.();
      return;
    }
    const msg = t("That staged action is no longer pending (it may already have been decided or expired).");
    setDeepLinkNotice(msg);
    onToast?.(msg);
    onClearDeepLinkAction?.();
  }, [deepLinkActionId, decisionsOn, staged.phase, staged.pending, staged.decided, onClearDeepLinkAction, onToast, t]);

  const openDecision = (key: string, opener?: HTMLElement) => {
    const stagedCard = cards.find((n) => n.key === key && n.action.type === "staged");
    if (stagedCard) {
      const queue = cards.flatMap((n) => (n.action.type === "staged" ? [n.action.id] : []));
      reviewStaged(queue, Math.max(queue.indexOf(stagedCard.action.type === "staged" ? stagedCard.action.id : ""), 0), opener ?? document.activeElement);
      return;
    }
    const need = readers.find((n) => n.key === key);
    if (!need) return;
    reviewOpening.current = true;
    closeDrawerImmediate();
    onOpenDecision?.(decisionFor(need, readers.filter((n) => n.key === key || !isDone(n))));
  };

  const openNeedCard = (need: HomeNeed, opener: HTMLElement) => {
    if (need.action.type === "staged" || kindSpec(need).place === "drawer") {
      openDecision(need.key, opener);
      return;
    }
    openTodoDetail({ kind: "need", key: need.key }, opener);
  };

  const renderUnifiedTodoItem = (item: UnifiedTodoItem) => {
    if (item.kind === "need") {
      return (
        <TodoCard
          key={item.key}
          need={item.need}
          doneText={marksFor(local, item.need).doneText}
          onOpen={openNeedCard}
        />
      );
    }
    if (item.kind === "routine_step_hold") {
      const { run } = item;
      const waitingStep =
        (run.checklist_state ?? run.checklist ?? []).find((s) => s.state === "waiting_approval")?.label ??
        t("Ask for approval first");
      return (
        <li key={item.key} className="needrow">
          <RailCard
            className="todo-card"
            bodyClassName="todo-main min-w-0"
            titleClassName="todo-hit"
            metaClassName="todo-meta"
            tone="warn"
            statusLabel={t("Approval")}
            source={run.agent_alias ?? t("Routine")}
            title={<span className="todo-title">{run.routine_name ?? run.routine_id}</span>}
            summary={waitingStep}
            meta={run.agent_alias ?? undefined}
            cue={t("Review")}
            onOpen={(event) => openTodoDetail({ kind: "routine_step_hold", run }, event.currentTarget)}
          />
        </li>
      );
    }
    if (item.kind === "routine_proposal") {
      const { proposal } = item;
      return (
        <li key={item.key} className="needrow">
          <RailCard
            className="todo-card"
            bodyClassName="todo-main min-w-0"
            titleClassName="todo-hit"
            metaClassName="todo-meta"
            tone="warn"
            statusLabel={t("Approval")}
            source={proposal.proposed_by ?? t("Agent")}
            title={<span className="todo-title">{proposal.title}</span>}
            summary={t(proposal.kind === "wiki_propose" ? "Playbook draft" : "Routine draft")}
            cue={t("Review")}
            onOpen={(event) => openTodoDetail({ kind: "routine_proposal", proposal }, event.currentTarget)}
          />
        </li>
      );
    }
    if (item.kind === "waiting_routine") {
      const { routine } = item;
      const reason = waitingRoutineReason(routine, t);
      return (
        <li key={item.key} className="needrow">
          <RailCard
            className="todo-card"
            bodyClassName="todo-main min-w-0"
            titleClassName="todo-hit"
            metaClassName="todo-meta"
            tone="info"
            statusLabel={t("Review")}
            source={routine.agent_alias ?? t("Routine")}
            title={<span className="todo-title">{routine.name}</span>}
            summary={reason}
            cue={t("Review")}
            onOpen={(event) => openTodoDetail({ kind: "waiting_routine", routine }, event.currentTarget)}
          />
        </li>
      );
    }
    if (item.kind === "document") {
      const { doc } = item;
      return (
        <li key={item.key} className="needrow">
          <RailCard
            className="todo-card"
            bodyClassName="todo-main min-w-0"
            titleClassName="todo-hit"
            metaClassName="todo-meta"
            tone="info"
            statusLabel={t("Review")}
            source={doc.project ?? t("Wiki")}
            title={<span className="todo-title">{doc.title}</span>}
            summary={`${doc.project ? `${doc.project} · ` : ""}v${doc.version}`}
            meta={t("Approval requested")}
            cue={t("Review")}
            onOpen={(event) => openTodoDetail({ kind: "document", doc }, event.currentTarget)}
          />
        </li>
      );
    }
    if (item.kind === "local_assignment") {
      const entry = item.item;
      const pres = localAssignmentPresentation(entry);
      return (
        <li key={item.key} className="needrow" data-local-assignment-issue={entry.issue_id}>
          <RailCard
            className="todo-card"
            bodyClassName="todo-main min-w-0"
            titleClassName="todo-hit"
            metaClassName="todo-meta"
            tone={pres.tone}
            statusLabel={t(pres.statusLabel)}
            source={pres.sourceLabel}
            title={<span className="todo-title">{entry.issue_id} · {entry.issue_title}</span>}
            summary={pres.summaryText}
            meta={pres.metaLabel}
            cue={t(pres.cueLabel)}
            onOpen={(event) => openTodoDetail({ kind: "local_assignment", item: entry }, event.currentTarget)}
          />
        </li>
      );
    }
    const { template } = item;
    return (
      <li key={item.key} className="needrow" data-record-id={`template:${template.name}:${template.version}`}>
        <RailCard
          className="todo-card"
          bodyClassName="todo-main min-w-0"
          titleClassName="todo-hit"
          metaClassName="todo-meta"
          tone="info"
          statusLabel={t("Review")}
          source={t("Templates")}
          title={<span className="todo-title">{template.name} · v{template.version}</span>}
          summary={t("Draft awaiting review")}
          meta={t(template.kind)}
          cue={t("Review")}
          onOpen={(event) => {
            reviewOpening.current = true;
            closeDrawerImmediate();
            templateList?.open(template, event.currentTarget);
          }}
        />
      </li>
    );
  };

  const todoSubhead = todoCategories.length > 1 ? (
    <div className="rail-subhead">
      <div className="seg-pills" role="group" aria-label={t("Filter to do")}>
        <button
          type="button"
          className="seg-pill"
          aria-pressed={todoFilter === "all"}
          onClick={() => setTodoFilter("all")}
        >
          {t("All")} <span className="seg-n num">{unifiedTodos.length}</span>
        </button>
        {todoCategories.map((entry) => (
          <button
            key={entry.key}
            type="button"
            className="seg-pill"
            aria-pressed={todoFilter === entry.key}
            onClick={() => setTodoFilter(entry.key)}
          >
            {t(entry.label)} <span className="seg-n num">{entry.count}</span>
          </button>
        ))}
      </div>
    </div>
  ) : null;

  const todoBody = (
    <>
      {!templateList && templateRail?.(() => {
        if (!drawer) return;
        reviewOpening.current = true;
        closeDrawerImmediate();
      })}
      {templateList?.error && (
        <p className="rail-state-card warn" role="alert">
          {t("Templates could not be loaded.")}{" "}
          <button type="button" className="lnk" onClick={templateList.refresh}>{t("Retry")}</button>
        </p>
      )}
      {block && <p className="todo-note">{t(READ_ONLY_COPY)}</p>}
      {deepLinkNotice && review === null && (
        <p className="todo-note text-label text-ink-300 px-3 pt-2" role="status" data-deep-link-notice>
          <span>{deepLinkNotice}</span>{" "}
          <button type="button" className="lnk text-label" onClick={() => setDeepLinkNotice(null)}>
            {t("Dismiss")}
          </button>
        </p>
      )}
      {staged.notice && review === null && (
        <p className="todo-note text-label text-ink-300 px-3 pt-2" role="status">
          <span>{noticeText(staged.notice, t)}</span>
          {staged.notice.outcome !== "still-waiting" && (
            <button type="button" className="lnk text-label" onClick={() => showTab("feed")}>
              {t("View in Feed")}
            </button>
          )}
        </p>
      )}
      {overview && !overviewData && overview.status === "failed" && (
        <p className="rail-state-card fail">{t("The list couldn't be loaded. It will try again.")}</p>
      )}
      {overview && !overviewData && overview.status !== "failed" && (
        <p className="rail-state-card">{t("Reading what needs you…")}</p>
      )}
      {(overviewData || privateHome) && unifiedTodos.length === 0 && decisionsOn && (staged.phase === "idle" || staged.phase === "loading") && (
        <p className="rail-state-card">{t("Reading what needs you…")}</p>
      )}
      {(overviewData || privateHome) && unifiedTodos.length === 0 && decisionsOn && staged.phase === "error" && (
        <p className="rail-state-card fail" role="alert">{t("The list couldn't be loaded. It will try again.")}</p>
      )}
      {countKnown && count === 0 && unifiedTodos.length === 0 && documentFollowUps?.status !== "unavailable" && documentFollowUps?.status !== "loading" && !(decisionsOn && (staged.phase === "idle" || staged.phase === "loading" || staged.phase === "error")) && (
        <p className="rail-state-card">{t(privateHome ? "Nothing needs your review." : "Nothing needs you. The team is working.")}</p>
      )}
      {documentFollowUps?.status === "loading" && (
        <p className="rail-state-card">{t("Reading document follow-ups…")}</p>
      )}
      {documentFollowUps?.status === "unavailable" && (
        <p className="rail-state-card warn">
          {t("Document follow-ups could not be loaded.")}{" "}
          <button type="button" className="lnk" onClick={() => void resources.overview.refresh()}>{t("Retry")}</button>
        </p>
      )}

      {visibleTodos.length > 0 && (
        <ul className="rail-cards todo-list">
          {visibleTodos.map(renderUnifiedTodoItem)}
        </ul>
      )}
      {unifiedTodos.length > 0 && visibleTodos.length === 0 && (
        <p className="rail-state-card">{t("No items in this category.")}</p>
      )}

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
              {permissionsHref && (
                <Link href={permissionsHref} className="lnk text-label" onClick={closeDrawerImmediate}>
                  {t("Standing rules are in Settings")}
                </Link>
              )}
            </>
          )}
        </section>
      )}
    </>
  );

  const decidedSlice = useMemo(() => staged.decided.slice(0, 8), [staged.decided]);
  const nonAgentFeedCounts = useMemo(
    () =>
      countFeedCategories([
        ...recentDocuments.map((): { categories: readonly FeedCategoryKey[] } => ({ categories: ["uploaded", "completed"] })),
        ...approvedTemplates.map((): { categories: readonly FeedCategoryKey[] } => ({ categories: ["uploaded", "completed"] })),
        ...decidedSlice.map((w) => ({ categories: feedCategoriesForDecidedWrite(w) })),
        ...updates.map((n) => ({ categories: feedCategoriesForInfoNeed(n) })),
      ]),
    [recentDocuments, approvedTemplates, decidedSlice, updates],
  );
  const feedCounts = useMemo(
    () => mergeFeedCounts(agentFeedState.counts, nonAgentFeedCounts),
    [agentFeedState.counts, nonAgentFeedCounts],
  );

  const feedMatches = (categories: readonly FeedCategoryKey[]) =>
    feedFilter === "all" || categories.includes(feedFilter);

  const filteredRecentDocs = recentDocuments.filter(() => feedMatches(["uploaded", "completed"]));
  const filteredApprovedTemplates = approvedTemplates.filter(() => feedMatches(["uploaded", "completed"]));
  const filteredDecidedWrites = decidedSlice.filter((w) => feedMatches(feedCategoriesForDecidedWrite(w)));
  const filteredUpdates = updates.filter((n) => feedMatches(feedCategoriesForInfoNeed(n)));

  const feedAgentVisibleCount = feedFilter === "all" ? (agentFeedState.hasAny ? 1 : 0) : agentFeedState.counts[feedFilter];
  const feedVisibleCount =
    feedAgentVisibleCount +
    filteredRecentDocs.length +
    filteredApprovedTemplates.length +
    filteredDecidedWrites.length +
    filteredUpdates.length;

  const feedSubhead = (
    <div className="rail-subhead">
      <div className="seg-pills" role="group" aria-label={t("Filter feed")}>
        <button
          type="button"
          className="seg-pill"
          aria-pressed={feedFilter === "all"}
          onClick={() => setFeedFilter("all")}
        >
          {t("All")}
        </button>
        {FEED_CATEGORY_ORDER.map((entry) => (
          <button
            key={entry.key}
            type="button"
            className="seg-pill"
            aria-pressed={feedFilter === entry.key}
            onClick={() => setFeedFilter(entry.key)}
          >
            {t(entry.label)}
            {entry.key === "action" && feedCounts.action > 0 ? (
              <span className="seg-n num">{feedCounts.action}</span>
            ) : null}
          </button>
        ))}
      </div>
    </div>
  );

  const feedBody = (
    <>
      <AgentUpdates
        includeMasterReports={!privateHome}
        category={feedFilter}
        onAsk={onAskAgent ? (agent, update) => {
          closeDrawerImmediate();
          onAskAgent(agent, update);
        } : undefined}
        onAskFeedItem={onAskFeedItem ? (item) => {
          closeDrawerImmediate();
          onAskFeedItem(item);
        } : undefined}
        onOpenAgent={openFeedDetail}
        onOpenFeedItem={(item, opener) => openFeedDetail({ kind: "feed_item", item }, opener)}
        onAgentFeedState={setAgentFeedState}
      />

      {filteredRecentDocs.length > 0 && (
        <ul className="rail-cards" data-feed-docs>
          {filteredRecentDocs.map((doc: DocumentFollowUp & { outcome?: "approved" | "delivered" }) => {
            const outcome = doc.outcome === "delivered" ? "delivered" : "approved";
            const outcomeLabel = outcome === "delivered" ? t("Delivered") : t("Approved");
            return (
              <li key={`${doc.id}:${doc.version}`} className="needrow">
                <RailCard
                  tone="ok"
                  statusLabel={outcomeLabel}
                  source={doc.project ?? t("Wiki")}
                  title={doc.title}
                  summary={`${doc.project ? `${doc.project} · ` : ""}v${doc.version}`}
                  meta={t("Uploaded")}
                  cue={t("Details")}
                  onOpen={(event) => openFeedDetail({ kind: "document", doc: { ...doc, outcome } }, event.currentTarget)}
                />
              </li>
            );
          })}
        </ul>
      )}

      {filteredApprovedTemplates.length > 0 && (
        <ul className="rail-cards" data-feed-templates>
          {filteredApprovedTemplates.map((tpl) => (
            <li key={`${tpl.name}:${tpl.version}`} className="needrow" data-record-id={`template:${tpl.name}:${tpl.version}`}>
              <RailCard
                tone="ok"
                statusLabel={t("Approved")}
                source={t("Templates")}
                title={`${tpl.name} · v${tpl.version}`}
                summary={t("Approved for reuse")}
                meta={t(tpl.kind)}
                cue={t("Open")}
                onOpen={(event) => {
                  reviewOpening.current = true;
                  closeDrawerImmediate();
                  templateList?.open(tpl, event.currentTarget);
                }}
              />
            </li>
          ))}
        </ul>
      )}

      {filteredDecidedWrites.length > 0 && (
        <ul className="rail-cards" data-todo-decisions>
          {filteredDecidedWrites.map((write) => {
            const outcome = outcomeOf(write);
            const when = ageText(write.decidedAt ?? write.createdAt, t);
            const statusLabel = decidedWriteStatusLabel(write, outcome, t);
            const tone = outcome === "failed" ? "fail" : outcome === "unconfirmed" ? "warn" : "ok";
            const summary =
              outcome === "failed" || (outcome === "unconfirmed" && write.connected !== null && isUncertain(write))
                ? reasonText(failureReason(write), t)
                : write.target?.label ?? undefined;
            return (
              <li key={write.id} className="needrow" data-fail={outcome === "failed" ? "" : undefined}>
                <RailCard
                  className="decided-card"
                  tone={tone}
                  statusLabel={statusLabel}
                  source={write.app ? <bdi>{isolate(write.app)}</bdi> : t("Workspace")}
                  age={when}
                  title={<bdi>{isolate(titleOf(write, t))}</bdi>}
                  summary={summary ? <bdi>{isolate(summary)}</bdi> : undefined}
                  meta={write.connected !== null
                    ? <>{t("External")}{write.app ? <> · <bdi>{isolate(write.app)}</bdi></> : null}</>
                    : write.app
                      ? <bdi>{isolate(write.app)}</bdi>
                      : undefined}
                  external={write.connected !== null}
                  cue={t("Details")}
                  onOpen={(event) => openFeedDetail({ kind: "decided_write", write }, event.currentTarget)}
                />
              </li>
            );
          })}
        </ul>
      )}

      {filteredUpdates.length > 0 && (
        <ul className="rail-cards" data-todo-updates>
          {filteredUpdates.map((need) => (
            <li key={need.key} className="needrow">
              <RailCard
                tone="info"
                statusLabel={t("Update")}
                source={need.project && need.project !== "all" ? need.project : t("Workspace")}
                age={ageWords(need.age)}
                title={updateTitle(need)}
                summary={metaLine(need)}
                meta={need.issue ?? undefined}
                cue={t("Details")}
                onOpen={(event) => openFeedDetail({ kind: "info_need", need }, event.currentTarget)}
              />
            </li>
          ))}
        </ul>
      )}

      {privateHome && documentFollowUps?.status === "loading" && feedVisibleCount === 0 && (
        <p className="rail-state-card">{t("Reading document follow-ups…")}</p>
      )}
      {documentFollowUps?.status === "unavailable" && (
        <p className="rail-state-card warn">{t("Document follow-ups could not be loaded.")}</p>
      )}
      {feedVisibleCount === 0 && documentFollowUps?.status !== "loading" && (
        <p className="rail-state-card">
          {t(feedCounts.all === 0 && privateHome ? "No recent document activity." : "No activity in this category yet.")}
        </p>
      )}
    </>
  );

  const liveAgents = Boolean(hasLiveAgents || agentFeedState.liveCount > 0);
  const tabs = (
    <div className="todo-tabs" role="group" aria-label={t("Home rail")}>
      <button type="button" aria-pressed={tab === "todo"} onClick={() => showTab("todo")}>
        {t("To do")} <span className={`todo-count ${count ? "on" : ""}`}>{countKnown ? count : "…"}</span>
      </button>
      <button type="button" aria-pressed={tab === "feed"} onClick={() => showTab("feed")}>
        {t("Feed")}
        {liveAgents && <span className="live-dot" aria-hidden="true" />}
        {staged.failedUnseen && tab !== "feed" && (
          <span className="dx-dot" role="img" aria-label={t("A write was approved but did not apply")} />
        )}
      </button>
      {tab === "feed" && (
        <span className={`rail-stream-pill${streamDegraded ? " warn" : ""}`} aria-live="polite">
          {streamDegraded ? t("Poll 15s") : t("Live")}
        </span>
      )}
    </div>
  );

  const panelSubhead = tab === "todo" ? todoSubhead : feedSubhead;
  const panelBody = <div>{tab === "todo" ? todoBody : feedBody}</div>;

  return (
    <>
      {/* The desktop column scrolls independently of the conversation. */}
      <div className="hidden rail:block h-full min-h-0">
        <section className="needsrail overflow-hidden rounded-lg" aria-label={t("to do")}>
          {tabs}
          {panelSubhead}
          <div className="rail-scroll min-h-0" ref={desktopScrollRef}>{panelBody}</div>
        </section>
      </div>

      {/* <1100px: a floating button opens the slide-over drawer. */}
      <button
        type="button"
        className="needbtn rail:hidden"
        ref={needBtn}
        onClick={openDrawer}
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
          home
          onAt={(at) => setReview((r) => (r === null ? r : { ...r, at }))}
          onClose={closeStaged}
        />
      )}
      <RailTodoDrawer
        target={todoTarget}
        needs={cards}
        readOnly={readOnly}
        canLocalDispatch={canLocalDispatch}
        scopedHost={scopedHost}
        hostedRoster={hostedRoster}
        onOpenIssue={openIssue}
        onToast={onToast}
        onAsk={onAsk ? (need) => ask(need) : undefined}
        onDone={doneTodo}
        onHide={hideTodo}
        onSent={() => {
          closeDrawerImmediate();
          onSent?.();
        }}
        onRoutineDecided={decideRoutine}
        onClose={closeTodoDetail}
      />
      <RailFeedDrawer
        target={feedTarget}
        readOnly={readOnly}
        onAskAgent={onAskAgent ? (agent, update) => {
          closeDrawerImmediate();
          onAskAgent(agent, update);
        } : undefined}
        onAskFeedItem={onAskFeedItem ? (item) => {
          closeDrawerImmediate();
          onAskFeedItem(item);
        } : undefined}
        onAskNeed={onAsk ? (need) => ask(need) : undefined}
        onOpenIssue={openIssue}
        onClose={closeFeedDetail}
      />
      {drawerToken !== null && (
        <DrawerShell
          key={drawerToken}
          kind="needs"
          scrim="board"
          modal
          trapFocus
          size="sm"
          className="needsdrawer rail:hidden"
          scrimClassName="rail:hidden"
          closeLabel={t("close the rail")}
          label={t("to do")}
          title={t("To do")}
          tab={tab}
          onTab={(next) => showTab(next as "todo" | "feed")}
          tabs={[
            {
              id: "todo",
              label: `${t("To do")} ${countKnown ? count : "…"}`,
              panel: (
                <div className="needsdrawer-panel flex flex-col min-h-0 flex-1">
                  {todoSubhead}
                  <div className="rail-scroll min-h-0 flex-1" ref={drawerScrollRef}>{todoBody}</div>
                </div>
              ),
            },
            {
              id: "feed",
              label: t("Feed"),
              badge:
                liveAgents || (staged.failedUnseen && tab !== "feed") ? (
                  <span className="inline-flex items-center gap-1 ml-1.5">
                    {liveAgents && <span className="live-dot" aria-hidden="true" />}
                    {staged.failedUnseen && tab !== "feed" && (
                      <span className="dx-dot" role="img" aria-label={t("A write was approved but did not apply")} />
                    )}
                  </span>
                ) : undefined,
              panel: (
                <div className="needsdrawer-panel flex flex-col min-h-0 flex-1">
                  {feedSubhead}
                  <div className="rail-scroll min-h-0 flex-1" ref={drawerScrollRef}>{feedBody}</div>
                </div>
              ),
            },
          ]}
          onClose={() => {
            const closingToken = drawerToken;
            setDrawerToken((cur) => (cur === closingToken ? null : cur));
          }}
        />
      )}
    </>
  );
}
