import { useEffect, useId, useMemo, useRef, useState } from "react";
import SafeLink from "../../ui/SafeLink";
import Button from "../../ui/Button";
import Md from "../../ui/Md";
import Select from "../../ui/Select";
import type { ResourceState } from "../../lib/cache";
import { api, ApiError, type MyWorkIssue } from "../../lib/api";
import type { IssueDetail } from "../../lib/types";
import { useLocale } from "../../lib/locale";
import { fmtTime } from "../../lib/fmt";
import { privateReadsAllowed, privateSessionGeneration } from "../../lib/privateSession";
import { resources } from "../../lib/resources";
import {
  acceptanceItems,
  laneState,
  peekSummary,
  prRef,
  reportView,
  timelineRows,
  verdictStale,
} from "../issues/model";
import { PRIORITY_CHIP, STATUS_CHIP } from "../issues/chips";
import { noDragReason } from "../projects/Card";
import { Evidence } from "../issues/Evidence";
import DecisionPanel from "../home/DecisionPanel";
import AssignmentSummary from "../home/AssignmentSummary";
import DrawerShell from "../app-shell/shared/DrawerShell";
import type { HomeNeed } from "../home/needs";
import {
  workItemFacts,
  workItemOriginLabel,
  workItemTabs,
  type HomeDecisionOpen,
  type WorkItemFacts,
  type WorkItemOrigin,
  type WorkItemTabId,
  type MyWorkDraft,
  type MyWorkWorkItemProps,
} from "../../lib/workItems";
import "./workItems.css";

const IMG = /\.(png|jpe?g|gif|webp)$/i;
const STATUSES = ["backlog", "ready", "doing", "review", "done", "dropped"];

export interface WorkItemDetailProps {
  /** The issue id the detail renders. */
  id: string;
  /** Which surface opened it; closing returns there. */
  origin: WorkItemOrigin;
  detail: IssueDetail | null;
  readState?: ResourceState<IssueDetail> | null;
  onRetry?: () => void;
  /** Full issue page. Null when the project is unknown. */
  href: string | null;
  onClose: () => void;
  /** The write gate for the hosted Home decision panels; artifacts stay on the issue page. */
  readOnly: boolean;
  /** Open another issue inside this detail (body links). */
  onOpen: (id: string) => void;
  /** The My work row that opened it, when one did. */
  card?: MyWorkIssue | null;
  /** Opt-in My work actions. Absent ⇒ a read-only detail. */
  myWork?: MyWorkWorkItemProps;
  /** A Home decision hosted above the facts; its own guards stay in charge. */
  home?: {
    open: HomeDecisionOpen;
    /** `done` is the decision that completed — a late reply may not be the current one. */
    onDone: (done: HomeDecisionOpen, key: string, text: string) => void;
    onAsk: (need: HomeNeed, lead: string) => void;
  };
}

/**
 * CAD-1811 first slice: the one issue-backed work-item detail, opened from
 * Home decisions, My work rows and Threads references. It reads the issue
 * payload the board already authorized and shows recorded facts only —
 * Overview, Code (dev), Evidence, Approvals, Activity and Audit. A field the
 * issue does not record is labelled, never filled. Home decisions render
 * their existing guard-bound panel; My work keeps its bounded comment/status
 * writes. App owns the selection, the reads and the writes.
 */
export default function WorkItemDetail(props: WorkItemDetailProps) {
  const { t } = useLocale();
  const { id, origin, detail, readState, onRetry, href, onClose, readOnly, onOpen, card, myWork, home } = props;
  const facts = useMemo(() => workItemFacts(detail, card ?? null), [detail, card]);
  const known = detail !== null && detail.id === id;
  const title = facts.title ?? (home ? home.open.need.shortTitle ?? t("Decision") : t("Work item"));
  const labels: Record<WorkItemTabId, string> = {
    overview: "Overview",
    code: "Code",
    evidence: "Evidence",
    approvals: "Approvals",
    activity: "Activity",
    audit: "Audit",
  };
  const tabs = workItemTabs(detail).map((tabId) => ({
    id: tabId,
    label: t(labels[tabId]),
    panel:
      tabId === "overview" ? (
        <OverviewPanel
          id={id}
          facts={facts}
          detail={known ? detail : null}
          readState={readState}
          onRetry={onRetry}
          onOpen={onOpen}
          onClose={onClose}
          myWork={myWork}
          readOnly={readOnly}
          home={home}
        />
      ) : tabId === "code" ? (
        <CodePanel detail={known ? detail : null} />
      ) : tabId === "evidence" ? (
        <EvidencePanel detail={known ? detail : null} />
      ) : tabId === "approvals" ? (
        <ApprovalsPanel detail={known ? detail : null} />
      ) : tabId === "activity" ? (
        <ActivityPanel detail={known ? detail : null} />
      ) : (
        <AuditPanel detail={known ? detail : null} />
      ),
  }));

  return (
    <DrawerShell
      kind={origin === "home" ? "review" : "work-item"}
      scrim="board"
      label={t("Work item")}
      kicker={t(workItemOriginLabel(origin))}
      title={title}
      subtitle={
        <>
          <span className="num">{id}</span>
          {facts.project ? ` · ${facts.project}` : ""}
        </>
      }
      pills={facts.status ? <Pills facts={facts} /> : undefined}
      warning={
        facts.blocked ? (
          <>
            <strong>{t("Blocked.")}</strong> {facts.blockedReason ?? t("This item cannot proceed until the block is cleared.")}
          </>
        ) : undefined
      }
      tabs={tabs}
      foot={
        <>
          <div className="crm-drawer-actions">
            {href ? (
              <Button variant="primary" href={href}>{t("Open full issue")}</Button>
            ) : (
              <Button disabled title={t("Project unknown")}>{t("Open full issue")}</Button>
            )}
          </div>
          <span className="crm-drawer-note">{t(workItemOriginLabel(origin))}</span>
        </>
      }
      onClose={onClose}
    />
  );
}

function Pills({ facts }: { facts: WorkItemFacts }) {
  const { t } = useLocale();
  return (
    <>
      {facts.status && <span className={`chip ${STATUS_CHIP[facts.status] ?? "bg-ink-800 text-ink-400"}`}>{t(facts.status)}</span>}
      {facts.priority && <span className={`chip ${PRIORITY_CHIP[facts.priority] ?? "bg-ink-800 text-ink-400"}`}>{facts.priority}</span>}
      {facts.blocked && <span className="chip bg-warn/15 text-warn">{t("Blocked")}</span>}
      {(facts.parent || facts.component) && <span className="chip bg-ink-800 text-ink-400">{facts.parent ?? facts.component}</span>}
    </>
  );
}

function ReadState({ readState, onRetry }: { readState?: ResourceState<IssueDetail> | null; onRetry?: () => void }) {
  const { t } = useLocale();
  if (!readState) return null;
  if (readState.inFlight) {
    return (
      <p className="issue-peek-note dim m-0" role="status">
        {readState.data === null ? t("Loading item…") : t("Refreshing; showing the saved details.")}
      </p>
    );
  }
  if (readState.status === "stale") {
    return (
      <div className="flex items-center gap-2 flex-wrap" role="status">
        <span className="issue-peek-note warn m-0">{t("Showing saved details; they may be out of date.")}</span>
        {onRetry && <Button size="sm" onClick={onRetry}>{t("Retry")}</Button>}
      </div>
    );
  }
  if (readState.status === "failed" || readState.data === null) {
    if (!readState.error) return <p className="issue-peek-note dim m-0" role="status">{t("Loading item…")}</p>;
    return (
      <div className="issue-peek-note err m-0" role="alert">
        <p className="m-0">
          {readState.discarded
            ? t("This issue is no longer available to you — private details and any draft were cleared.")
            : `${t("Could not load this item")}: ${readState.error}`}
        </p>
        {onRetry && <Button className="mt-2" size="sm" onClick={onRetry}>{t("Retry")}</Button>}
        <p className="mt-2 m-0 dim">{t("The full issue link below still works while project access allows.")}</p>
      </div>
    );
  }
  return null;
}

function Fact({ label, value }: { label: string; value: string | null }) {
  const { t } = useLocale();
  return (
    <>
      <dt className="slabel">{t(label)}</dt>
      <dd className={value ? "text-secondary text-ink-200 min-w-0" : "text-secondary text-ink-500 min-w-0"}>
        {value ?? t("Not recorded")}
      </dd>
    </>
  );
}

function OverviewPanel({
  id,
  facts,
  detail,
  readState,
  onRetry,
  onOpen,
  onClose,
  myWork,
  readOnly,
  home,
}: {
  id: string;
  facts: WorkItemFacts;
  detail: IssueDetail | null;
  readState?: ResourceState<IssueDetail> | null;
  onRetry?: () => void;
  onOpen: (id: string) => void;
  onClose: () => void;
  myWork?: MyWorkWorkItemProps;
  readOnly: boolean;
  home?: WorkItemDetailProps["home"];
}) {
  const { t } = useLocale();
  const [requirementsOpen, setRequirementsOpen] = useState(false);
  const body = detail?.body ?? "";
  const items = useMemo(() => acceptanceItems(body), [body]);
  const checksDone = items.reduce((count, item) => count + Number(item.checked), 0);
  const agents = detail?.agents ?? [];
  const lane = agents.length === 0 ? t("No lane yet") : agents.map((a) => a.alias).join(", ");
  const owner = myWork && facts.owner === myWork.owner ? t("you") : facts.owner ?? t("unassigned");
  const gone = Boolean(
    myWork &&
      detail &&
      (myWork.membership === "unassigned" ||
        (myWork.membership === "assigned" && detail.owner !== myWork.owner)),
  );

  return (
    <div className="wi-panel">
      {home && (
        <DecisionPanel
          key={home.open.need.key}
          need={home.open.need}
          readOnly={readOnly}
          index={home.open.index}
          total={home.open.total}
          onDone={(key, text) => home.onDone(home.open, key, text)}
          onClose={onClose}
          onAsk={home.onAsk}
        />
      )}

      <ReadState readState={readState} onRetry={onRetry} />

      <section className="grid gap-2" aria-label={t("Recorded details")}>
        <h4 className="rv-h">{t("What this item records")}</h4>
        {!facts.recorded && (
          <p className="issue-peek-note dim m-0">{t("Only the opening row is available; the full details have not loaded.")}</p>
        )}
        <dl className="grid grid-cols-[104px_minmax(0,1fr)] gap-x-2 gap-y-1.5">
          <Fact label="Status" value={facts.status ? t(facts.status) : null} />
          <Fact label="Priority" value={facts.priority} />
          <Fact label="Owner" value={facts.owner || myWork?.owner ? owner : null} />
          <Fact label="Type" value={facts.type} />
          <Fact label="Size" value={facts.size} />
          <Fact label="Milestone" value={facts.milestone} />
          <Fact label="Parent" value={facts.parent} />
          <Fact label="Component" value={facts.component} />
          <Fact
            label="Checks"
            value={`${facts.checks.done}/${facts.checks.total} ${facts.checks.total === 1 ? t("check") : t("checks")}`}
          />
          <Fact label="Comments" value={String(facts.comments)} />
          {detail && (
            <>
              <dt className="slabel">{t("Lane")}</dt>
              <dd className="text-secondary text-ink-200 min-w-0 truncate" title={lane}>
                {lane}
                {agents.length > 0 ? ` · ${laneState(agents)}` : ""}
              </dd>
            </>
          )}
          {myWork && (
            <>
              <dt className="slabel">{t("Assignment")}</dt>
              <dd className="text-secondary text-ink-200 min-w-0">
                <AssignmentSummary assignment={myWork.assignment} full />
              </dd>
            </>
          )}
        </dl>
      </section>

      <section className="grid gap-1" aria-label={t("Policy and effect")}>
        <h4 className="rv-h">{t("Policy and effect")}</h4>
        <p className="rv-note">
          {t("No policy is recorded on this issue. Approvals and merges keep their own pinned-head paths; this detail adds none.")}
        </p>
        <p className="rv-note">{t("No external effect is recorded on this issue.")}</p>
      </section>

      {gone && (
        <div role="status" className="grid gap-1">
          <p className="issue-peek-note warn m-0">{t("This issue is no longer assigned to you — quick actions are off.")}</p>
          <p className="issue-peek-note dim m-0">{t("It has left My work. You can still open the full issue while project access allows.")}</p>
        </div>
      )}
      {myWork && detail && !gone && (
        <QuickActions key={myWork.draftKey} id={id} detail={detail} readState={readState} personal={myWork} />
      )}

      {detail && (
        <section className="grid gap-2" aria-label={t("Summary")}>
          <h4 className="rv-h">{t("Summary")}</h4>
          <p className="text-secondary text-ink-300 m-0">{peekSummary(body)}</p>
          {items.length > 0 && (
            <p className="issue-peek-note dim m-0">
              {t("Acceptance")}: {checksDone} {t("of")} {items.length} {t("complete")}
            </p>
          )}
          <details className="issue-peek-sec" onToggle={(event) => setRequirementsOpen(event.currentTarget.open)}>
            <summary><span className="slabel">{t("Requirements")}</span></summary>
            {requirementsOpen && (
              body.trim() ? (
                <div className="issue-reader text-secondary text-ink-300"><Md text={body} onOpen={onOpen} /></div>
              ) : (
                <p className="issue-peek-note dim m-0">{t("No requirements recorded.")}</p>
              )
            )}
          </details>
        </section>
      )}

    </div>
  );
}

function CodePanel({ detail }: { detail: IssueDetail | null }) {
  const { t } = useLocale();
  if (!detail) return <p className="issue-peek-note dim m-0">{t("Code is listed once the item loads.")}</p>;
  const pr = prRef(detail.refs);
  const commits = detail.commits ?? [];
  return (
    <div className="wi-panel">
      <section className="grid gap-2" aria-label={t("Pull request")}>
        <h4 className="rv-h">{t("Pull request")}</h4>
        {pr ? (
          <>
            <p className="text-secondary text-ink-200 m-0">{pr.label}</p>
            {pr.href ? (
              <SafeLink className="lnk text-secondary break-all" href={pr.href}>{pr.href}</SafeLink>
            ) : (
              <p className="issue-peek-note dim m-0">{t("This reference has no URL.")}</p>
            )}
          </>
        ) : (
          <p className="issue-peek-note dim m-0">{t("No pull request is recorded on this issue.")}</p>
        )}
        <p className="issue-peek-note dim m-0">
          {t("Checks")}: {detail.checks.done}/{detail.checks.total}
        </p>
      </section>
      <section className="grid gap-2" aria-label={t("Commits")}>
        <h4 className="rv-h">{t("Commits")}</h4>
        {commits.length === 0 ? (
          <p className="issue-peek-note dim m-0">{t("No commits are recorded on this issue.")}</p>
        ) : (
          <ul className="issue-peek-list">
            {commits.map((commit) => (
              <li key={commit.sha} className="issue-peek-act">
                <div>
                  <span className="who num">{commit.sha.slice(0, 12)}</span>
                  {commit.at && <span className="when"> · {fmtTime(commit.at)}</span>}
                </div>
                <div>{commit.subject}</div>
                <div className="dim">{commit.repo}</div>
              </li>
            ))}
          </ul>
        )}
        {(detail.commits_skipped ?? []).length > 0 && (
          <ul className="issue-peek-list">
            {(detail.commits_skipped ?? []).map((skipped) => (
              <li key={skipped.repo} className="issue-peek-note dim m-0">
                {t("Commits not read")}: {skipped.repo} — {skipped.reason}
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}

/** Recorded evidence only: artifact authoring stays on the canonical issue page. */
function EvidencePanel({ detail }: { detail: IssueDetail | null }) {
  const { t } = useLocale();
  if (!detail) return <p className="issue-peek-note dim m-0">{t("Evidence is listed once the item loads.")}</p>;
  const artifacts = detail.artifacts ?? [];
  return (
    <div className="wi-panel">
      <Evidence
        id={detail.id}
        images={artifacts.filter((file) => IMG.test(file.name))}
        files={artifacts.filter((file) => !IMG.test(file.name))}
        readOnly
      />
    </div>
  );
}

function ApprovalsPanel({ detail }: { detail: IssueDetail | null }) {
  const { t } = useLocale();
  if (!detail) return <p className="issue-peek-note dim m-0">{t("Approvals are listed once the item loads.")}</p>;
  const reports = detail.reports ?? [];
  const verdicts = reports.filter((report) => report.kind === "verdict");
  const latest = reportView(reports).verdict;
  const stale = latest ? verdictStale(latest, detail) : false;
  return (
    <div className="wi-panel">
      <section className="grid gap-2" aria-label={t("Recorded reviews")}>
        <h4 className="rv-h">{t("Recorded reviews")}</h4>
        {verdicts.length === 0 ? (
          <p className="issue-peek-note dim m-0">{t("No revision-bound verdict is recorded on this issue.")}</p>
        ) : (
          <ul className="issue-peek-list">
            {verdicts.map((verdict) => (
              <li key={verdict.name} className="issue-peek-act">
                <div>
                  <span className="who">{verdict.agent ?? verdict.name}</span>
                  {verdict.at && <span className="when"> · {fmtTime(verdict.at)}</span>}
                </div>
                <div>
                  {verdict.verdict ?? t("verdict not recorded")}
                  {verdict.sha ? ` · ${t("applies to")} ${verdict.sha.slice(0, 12)}` : ` · ${t("no head recorded")}`}
                </div>
              </li>
            ))}
          </ul>
        )}
        {stale && (
          <p className="issue-peek-note warn m-0">
            {t("The latest verdict does not cover the current recorded head — it needs a fresh review.")}
          </p>
        )}
      </section>
      <section className="grid gap-1">
        <h4 className="rv-h">{t("Decisions")}</h4>
        <p className="rv-note">
          {t("Plan, idea and merge decisions open from Home and keep their own operator-only guards. This detail records no approval and adds no approval path.")}
        </p>
      </section>
    </div>
  );
}

function ActivityPanel({ detail }: { detail: IssueDetail | null }) {
  const { t, formatRelativeTime } = useLocale();
  if (!detail) return <p className="issue-peek-note dim m-0">{t("Activity is listed once the item loads.")}</p>;
  const rows = timelineRows(detail, []).slice(0, 30);
  return (
    <div className="wi-panel">
      <section className="grid gap-2" aria-label={t("Recorded activity")}>
        <h4 className="rv-h">{t("Recorded activity")}</h4>
        {rows.length === 0 ? (
          <p className="issue-peek-note dim m-0">{t("No activity is recorded on this issue.")}</p>
        ) : (
          <div className="grid gap-1.5">
            {rows.map((row, index) => (
              <div key={index} className="issue-peek-act">
                <div>
                  <span className="who">{row.title}</span>
                  {row.at && <span className="when"> · {formatRelativeTime(row.at)}</span>}
                </div>
                <div>{row.detail}</div>
              </div>
            ))}
          </div>
        )}
      </section>
    </div>
  );
}

function AuditPanel({ detail }: { detail: IssueDetail | null }) {
  const { t } = useLocale();
  if (!detail) return <p className="issue-peek-note dim m-0">{t("Audit records are listed once the item loads.")}</p>;
  const chain = [...(detail.notes_chain ?? [])].sort(
    (a, b) => (Date.parse(b.at) || 0) - (Date.parse(a.at) || 0),
  );
  return (
    <div className="wi-panel">
      <section className="grid gap-2" aria-label={t("Audit")}>
        <h4 className="rv-h">{t("Recorded notes chain")}</h4>
        <p className="rv-note">
          {t("Cadence keeps this issue's changes in its notes chain. This board records no separate audit ledger for the item.")}
        </p>
        {chain.length === 0 ? (
          <p className="issue-peek-note dim m-0">{t("No audit records on this issue.")}</p>
        ) : (
          <ul className="issue-peek-list">
            {chain.map((entry) => (
              <li key={entry.name} className="issue-peek-act">
                <div>
                  <span className="who">{t(entry.kind)}</span>
                  {entry.at && <span className="when"> · {fmtTime(entry.at)}</span>}
                </div>
                <div>{entry.title}</div>
                <div className="num dim">{entry.name}</div>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}

interface Outcome {
  tone: "ok" | "err" | "warn" | "dim";
  text: string;
}

/** Why the bounded writes are off, from recorded state only. */
function writeWhy(
  personal: MyWorkWorkItemProps,
  detail: IssueDetail,
  readState: ResourceState<IssueDetail> | null | undefined,
  t: (s: string) => string,
): string | null {
  // Owner "" while checking means the App has no verified owner yet — the
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

/**
 * The My work quick actions, carried over from the issue peek unchanged in
 * behaviour: one bounded comment post or explicit status Save, drafts kept
 * across close/reopen per identity, and the same revision and membership
 * guards. The detail only renders it for the My work origin.
 */
function QuickActions({
  id,
  detail,
  readState,
  personal,
}: {
  id: string;
  detail: IssueDetail;
  readState: ResourceState<IssueDetail> | null | undefined;
  personal: MyWorkWorkItemProps;
}) {
  const { t } = useLocale();
  const panelRef = useRef<HTMLElement | null>(null);
  const stored = personal.drafts.get(personal.draftKey);
  const [draft, setDraft] = useState<Pick<MyWorkDraft, "comment" | "status">>(() => ({ comment: stored?.comment ?? "", status: stored?.status ?? null }));
  useEffect(() => {
    const next = stored ?? { comment: "", status: null };
    setDraft((previous) => previous.comment === next.comment && previous.status === next.status ? previous : { comment: next.comment, status: next.status });
  }, [stored]);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const saving = stored?.pending?.kind ?? null;
  const denied = readState?.discarded === true;
  useEffect(() => {
    if (!denied) return;
    if (personal.drafts.delete(personal.draftKey)) personal.onDraftChange();
    setDraft((previous) => previous.comment || previous.status ? { comment: "", status: null } : previous);
    setOutcome(null);
  }, [denied, personal]);
  // Mounting only controls DOM focus. Shared submission tokens, identity and
  // private-session generation govern asynchronous state across close/reopen.
  const mounted = useRef(true);
  const ctxRef = useRef({ id, key: personal.identityKey });
  ctxRef.current = { id, key: personal.identityKey };
  const commentId = useId();
  const statusId = useId();
  const pendingFocus = useRef<{ kind: "comment" | "status"; key: string | null; generation: number } | null>(null);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      pendingFocus.current = null;
    };
  }, []);

  const updateDraft = (change: Partial<Pick<MyWorkDraft, "comment" | "status">>) => {
    const current: MyWorkDraft = personal.drafts.get(personal.draftKey) ?? draft;
    if (current.pending) return;
    const next = { ...current, ...change };
    if (next.comment === "" && next.status === null) personal.drafts.delete(personal.draftKey);
    else personal.drafts.set(personal.draftKey, next);
    setDraft({ comment: next.comment, status: next.status });
  };

  /** Submission owns focus through revalidation only until the user moves away. */
  useEffect(() => {
    const request = pendingFocus.current;
    if (!request) return;
    if (!mounted.current || !privateReadsAllowed() || request.key !== ctxRef.current.key ||
        request.generation !== privateSessionGeneration() || panelRef.current === null) {
      pendingFocus.current = null;
      return;
    }
    const issueRead = resources.issue(id).get();
    const personalRead = resources.myWorkIssue(id).get();
    if (saving !== null || issueRead.inFlight || personalRead.inFlight) return;
    if (issueRead.status !== "ok" || personalRead.status !== "ok" || personal.membership === "unassigned") {
      pendingFocus.current = null;
      return;
    }
    if (readState?.inFlight || personal.membership === "checking") return;
    const target = document.getElementById(request.kind === "comment" ? commentId : statusId);
    if (!(target instanceof HTMLTextAreaElement || target instanceof HTMLButtonElement) || target.disabled || !document.hasFocus()) {
      pendingFocus.current = null;
      return;
    }
    if (document.activeElement === target) return;
    const frame = requestAnimationFrame(() => {
      if (pendingFocus.current !== request) return;
      if (resources.issue(id).get().inFlight || resources.myWorkIssue(id).get().inFlight || target.disabled) return;
      if (mounted.current && privateReadsAllowed() && request.key === ctxRef.current.key &&
          request.generation === privateSessionGeneration() && target.isConnected &&
          panelRef.current !== null && document.hasFocus()) target.focus({ preventScroll: true });
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
    if (!detail || denied || personal.drafts.get(personal.draftKey)?.pending) return;
    if (personal.membership !== "assigned" || detail.owner !== personal.owner) return;
    const liveRead = resources.issue(id).get();
    const livePersonal = resources.myWorkIssue(id).get();
    if (!privateReadsAllowed() || !liveRead.data || liveRead.discarded || liveRead.inFlight ||
        liveRead.status !== "ok" || liveRead.data.id !== id || liveRead.data.rev !== detail.rev ||
        !livePersonal.data || livePersonal.inFlight || livePersonal.status !== "ok" ||
        livePersonal.data.owner !== personal.owner || livePersonal.data.id !== id || !livePersonal.data.assigned) return;
    if (writeWhy(personal, liveRead.data, liveRead, t) !== null) return;
    if (kind === "status" && noDragReason(liveRead.data) !== null) return;
    const body = draft.comment.trim();
    const next = draft.status;
    if (kind === "comment" && !body) return;
    if (kind === "status" && (!next || next === detail.status)) return;
    const ctx = {
      id: detail.id, rev: detail.rev, identityKey: personal.identityKey,
      generation: privateSessionGeneration(), token: Symbol("work-item-write"), body, status: next,
    };
    const sameScope = () => privateSessionGeneration() === ctx.generation &&
      ctxRef.current.id === ctx.id && ctxRef.current.key === ctx.identityKey;
    personal.drafts.set(personal.draftKey, { ...draft, pending: { token: ctx.token, kind } });
    if (mounted.current) pendingFocus.current = { kind, key: ctx.identityKey, generation: ctx.generation };
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
        void resources.myWorkIssue(ctx.id).invalidate();
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
          void resources.myWorkIssue(ctx.id).invalidate();
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

  const why = writeWhy(personal, detail, readState, t);
  const statusWhy = why ?? noDragReason(detail);
  const canComment = Boolean(draft.comment.trim());
  const statusChanged = draft.status !== null && draft.status !== detail.status;
  const draftKept = Boolean(draft.comment.trim()) || statusChanged;
  return (
    <section
      ref={panelRef}
      className="issue-peek-actions wi-quick-actions"
      aria-label={t("Quick actions")}
      onFocusCapture={(event) => releaseFocusOutside(event.target)}
      onPointerDownCapture={(event) => releaseFocusOutside(event.target)}
    >
      <span className="slabel">{t("Quick actions")}</span>
      <label className="slabel" htmlFor={commentId}>{t("Add a comment")}</label>
      <textarea
        id={commentId}
        className="field w-full"
        rows={3}
        value={draft.comment}
        placeholder={t("Write a comment…")}
        disabled={Boolean(why) || saving !== null}
        onChange={(event) => updateDraft({ comment: event.target.value })}
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
            ...STATUSES.map((status) => ({ value: status, label: t(status) })),
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
      {outcome && (
        <p className={`issue-peek-note ${outcome.tone} m-0`} role={outcome.tone === "err" ? "alert" : "status"}>{outcome.text}</p>
      )}
    </section>
  );
}
