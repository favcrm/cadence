import { useContext, useId, useRef, type ReactNode } from "react";
import SafeLink from "../../ui/SafeLink";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import Md from "../../ui/Md";
import { fmtTime } from "../../lib/fmt";
import { useLocale } from "../../lib/locale";
import type { IssueDetail, TaskReport } from "../../lib/types";
import { composerBlocked, type LanePayload } from "./lane";
import { WriteGate } from "../auth/WriteGate";
import { isolate, outcomeOf, type StagedWrite } from "../decisions/stagedWrite";
import {
  deployRefs,
  hostedAssignmentText,
  latestHostedAssignment,
  matchingStagedActions,
  mergeState,
  prRef,
  reportView,
  shownLinks,
  verdictStale,
  type IssueTab,
} from "./model";

const TONE_DOT: Record<string, string> = {
  done: "bg-ok",
  now: "bg-accent",
  warn: "bg-warn",
  wait: "bg-ink-600",
};

/** What an operator should do next, derived only from recorded state. */
function nextAction({
  detail,
  lane,
  questions,
  verdict,
  checksTotal,
  blocked,
  hosted,
  inspectPr,
  t,
}: {
  detail: IssueDetail;
  /** Null while the lane resource is loading or failed — not "no lane". */
  lane: LanePayload | null;
  questions: TaskReport[];
  verdict: TaskReport | null;
  checksTotal: number;
  blocked: string | null;
  hosted: boolean;
  inspectPr: boolean;
  t: (s: string) => string;
}): string {
  if (detail.status === "done" || detail.status === "dropped")
    return t("The issue is closed; nothing is pending here.");
  if (hosted && !lane?.lane) {
    if (detail.blocked) {
      return detail.blocked_reason
        ? `${t("Blocked.")} ${detail.blocked_reason}`
        : t("This item cannot proceed until the block is cleared.");
    }
    if (questions.length > 0)
      return `${t("Answer the open question")} — ${questions[0].body?.split("\n")[0] ?? questions[0].name}`;
    if (verdict?.verdict === "revise")
      return t("The independent review asked for a revision.");
    const latestAssign = latestHostedAssignment(detail);
    const runState = detail.agent_run?.state;
    if (detail.status === "review" || latestAssign?.state === "result" || runState === "finished")
      return t("Review the available evidence and acceptance checks.");
    if (
      latestAssign &&
      (latestAssign.state === "blocked" || latestAssign.state === "failed" || latestAssign.state === "cancelled")
    ) {
      return t("The latest assignment stopped — review Activity or reassign.");
    }
    if (
      runState &&
      (runState === "failed" || runState === "refused" || runState === "stopped" || runState === "no_report")
    ) {
      return t("The latest agent run stopped — review Activity or reassign.");
    }
    if (latestAssign?.state === "running" || runState === "running")
      return t("Check Activity or Evidence for updates on the assignment.");
    if (latestAssign?.state === "waiting")
      return t("Choose who should take this assignment.");
    if (latestAssign?.state === "queued" || runState === "queued")
      return t("Work is queued. Check Activity for updates.");
    if (inspectPr)
      return t("Inspect the linked pull request before starting new work.");
    if (detail.local_assignment && !detail.local_assignment.available && detail.local_assignment.reason !== "not_supported")
      return t("Assignment state unavailable");
    if (checksTotal === 0)
      return t("Add acceptance checks before starting work.");
    if (!detail.owner && !latestAssign && !detail.agent_run?.agent && !detail.agent_run?.agent_name)
      return t("Assign an owner before starting work.");
    return t("Review the brief and acceptance checks before starting work.");
  }
  if (checksTotal === 0)
    return t("Add at least one acceptance check — kick off stays blocked until one exists.");
  // `blocked` carries the real reason (write gate or dispatch block); show it.
  if (blocked) return t(blocked);
  if (lane === null)
    return t("The lane record is unavailable — check the lane card for its state.");
  const view = lane.lane;
  if (!view) return t("Kick off a lane to start the work.");
  if (view.state === "fenced")
    return t("Unfence or reassign the lane — the controls are on the lane card.");
  if (view.state === "quota" || view.state === "rate-limited")
    return t("The lane cannot take a turn — reassign is the suggested recovery.");
  if (questions.length > 0)
    return `${t("Answer the open question")} — ${questions[0].body?.split("\n")[0] ?? questions[0].name}`;
  if (verdict?.verdict === "revise")
    return t("The independent review asked for a revision — send an instruction in the conversation.");
  if (view.state === "busy")
    return t("The lane is working — ask for status or wait for the next report.");
  return t("Inspect the result below and leave feedback in the conversation.");
}

/**
 * CAD-1574 / CAD-2253 overview: objective, stage, next action, real output
 * links and the existing checks/reviews/approvals in one place. Non-repository
 * projects (`codeDelivery === false`) omit software-only Merge/Deployment rows
 * while surfacing recorded approvals and follow-up links cleanly.
 */
export function WorkResult({
  detail,
  lane,
  checksDone,
  checksTotal,
  blocked,
  hosted = false,
  codeDelivery,
  stagedActions,
  tabHref,
  onCompose,
  children,
  onOpen,
}: {
  detail: IssueDetail;
  lane: LanePayload | null;
  checksDone: number;
  checksTotal: number;
  blocked: string | null;
  hosted?: boolean;
  codeDelivery: boolean;
  stagedActions?: readonly StagedWrite[];
  tabHref: (tab: IssueTab) => string;
  onCompose: (mode: "ask" | "instruct") => void;
  children: ReactNode;
  onOpen: (id: string) => void;
}) {
  const { t, formatNumber } = useLocale();
  const briefRef = useRef<HTMLDivElement>(null);
  const verificationId = useId();
  const writeBlock = useContext(WriteGate);
  const view = reportView(detail.reports ?? []);
  const pr = prRef(detail.refs);
  const merge = mergeState(detail, pr);
  const deploys = deployRefs(detail.refs);
  const artifacts = detail.artifacts ?? [];
  const latest = view.latest;
  const verdict = view.verdict;
  const stale = verdict ? verdictStale(verdict, detail) : false;
  const laneView = lane?.lane ?? null;
  const workerOwn = latest?.agent && laneView?.agent === latest.agent;
  const feedbackBlocked = composerBlocked(laneView?.state) || writeBlock != null;
  const latestAssignment = latestHostedAssignment(detail);
  const approvalHeads = detail.approvals?.heads ?? [];
  const inForceHeads = approvalHeads.filter((h) => h.state === "in-force");
  const stagedMatches = matchingStagedActions(detail.id, stagedActions);
  const linkedRows = shownLinks(detail.links, detail.relates);
  const progress = checksTotal > 0
    ? `${formatNumber(checksDone)} ${t("of")} ${formatNumber(checksTotal)} ${t("checked")}`
    : null;
  const openChecklist = () => {
    const brief = briefRef.current;
    if (!brief) return;
    const heading = Array.from(brief.querySelectorAll<HTMLElement>(":scope > h2")).find(
      (el) => el.textContent?.trim().toLowerCase() === "acceptance",
    );
    const target = heading ?? brief;
    target.tabIndex = -1;
    target.focus({ preventScroll: true });
    target.scrollIntoView({ block: "start" });
  };

  const rows: { title: string; detail: ReactNode; tone: keyof typeof TONE_DOT }[] = [
    {
      title: t("Acceptance checks"),
      tone: checksTotal === 0 || checksDone < checksTotal ? "wait" : "done",
      detail: progress === null
        ? t("No acceptance checklist on the issue.")
        : <button type="button" className="lnk" onClick={openChecklist}>{t("Open checklist")}</button>,
    },
    {
      title: t("Worker report"),
      tone: latest || latestAssignment?.text || latestAssignment?.deliverable ? "now" : "wait",
      detail: latest
        ? `${latest.agent ?? t("unknown agent")} · ${latest.kind ?? "report"}${
            latest.at ? ` · ${fmtTime(latest.at)}` : ""
          }`
        : latestAssignment && (latestAssignment.text || latestAssignment.deliverable)
          ? (
              <>
                <bdi>{isolate(latestAssignment.target ?? detail.owner ?? t("Assigned"))}</bdi>
                {` · ${t(latestAssignment.state)}${
                  latestAssignment.updated_at ? ` · ${fmtTime(latestAssignment.updated_at)}` : ""
                }`}
              </>
            )
          : t("Nothing reported yet — no completion claim exists."),
    },
  ];

  if (
    approvalHeads.length > 0 ||
    stagedMatches.length > 0 ||
    (detail.approvals !== undefined && detail.approvals !== null && !detail.approvals.available) ||
    !codeDelivery
  ) {
    const approvalsUnavailable = Boolean(detail.approvals && !detail.approvals.available);
    const warnApprovals =
      approvalsUnavailable ||
      approvalHeads.some((h) => h.state === "unavailable") ||
      stagedMatches.some((s) => {
        const o = outcomeOf(s);
        return o === "pending" || o === "failed" || o === "unconfirmed";
      });
    const appliedStaged = stagedMatches.some((s) => outcomeOf(s) === "applied");
    rows.push({
      title: t("Approvals"),
      tone: warnApprovals
        ? "warn"
        : inForceHeads.length > 0 || appliedStaged
          ? "done"
          : "wait",
      detail:
        approvalsUnavailable && approvalHeads.length === 0 && stagedMatches.length === 0 ? (
          t("Approval records could not be read.")
        ) : approvalHeads.length > 0 || stagedMatches.length > 0 ? (
          <div className="grid gap-1">
            {approvalsUnavailable && (
              <p className="m-0 text-warn">{t("Approval records could not be read.")}</p>
            )}
            <ul className="m-0 p-0 list-none grid gap-1">
              {approvalHeads.map((head) => (
                <li
                  key={`${head.repo}#${head.pr}@${head.head_sha}`}
                  className="num text-secondary"
                >
                  <span>{`${head.repo} #${head.pr} @ ${head.head_sha.slice(0, 12)}`}</span>
                  {" · "}
                  <span>{t(head.state)}</span>
                  {head.source ? (
                    <>
                      {" · "}
                      <bdi>{isolate(head.source)}</bdi>
                    </>
                  ) : null}
                  {head.action ? ` (${head.action})` : ""}
                  {head.recorded_at ? ` · ${fmtTime(head.recorded_at)}` : ""}
                </li>
              ))}
              {stagedMatches.map((item) => (
                <li key={item.id} className="text-secondary">
                  <bdi>{isolate(item.title)}</bdi>
                  {" · "}
                  <span className="num">{t(outcomeOf(item))}</span>
                  {item.decidedAt || item.createdAt
                    ? ` · ${fmtTime(new Date(item.decidedAt ?? item.createdAt).toISOString())}`
                    : ""}
                </li>
              ))}
            </ul>
          </div>
        ) : (
          t("No approvals recorded on this item.")
        ),
    });
  }

  if (linkedRows.length > 0) {
    rows.push({
      title: t("Related & follow-ups"),
      tone: "now",
      detail: (
        <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1">
          {linkedRows.map((row) => (
            <span key={`${row.label}-${row.id}`} className="inline-flex items-center gap-1">
              <span className="text-ink-500">{t(row.label)}:</span>
              {row.missing ? (
                <span className="num text-ink-500">{row.id}</span>
              ) : (
                <button
                  type="button"
                  className="lnk num bg-transparent border-0 p-0 cursor-pointer"
                  onClick={() => onOpen(row.id)}
                >
                  {row.id}
                </button>
              )}
              {row.status ? <span className="text-ink-500">{`(${t(row.status)})`}</span> : null}
            </span>
          ))}
        </div>
      ),
    });
  }

  if (codeDelivery || verdict !== null) {
    rows.push({
      title: t("Independent review"),
      tone: !verdict ? "wait" : stale || verdict.verdict !== "pass" ? "warn" : "done",
      detail: !verdict ? (
        t("No revision-bound verdict is recorded.")
      ) : (
        <>
          {`${verdict.agent ?? t("unknown reviewer")} · ${verdict.verdict ?? "verdict"}${
            verdict.at ? ` · ${fmtTime(verdict.at)}` : ""
          }`}
          {verdict.sha && ` · ${t("applies to")} ${verdict.sha.slice(0, 12)}`}
          {stale && (
            <p className="m-0 text-ink-400">
              {t("Review coverage is unconfirmed — compare the recorded revision with newer evidence.")}
            </p>
          )}
        </>
      ),
    });
  }

  if (codeDelivery) {
    rows.push(
      {
        title: t("Merge"),
        tone: merge.tone,
        detail:
          (merge.kind === "merged"
            ? `${formatNumber(merge.count)} ${
                merge.count === 1 ? t("commit") : t("commits")
              } ${t("reachable from the default branch")}.`
            : merge.kind === "not_default"
              ? t("Linked commits are not on the default branch.")
              : merge.kind === "unknown"
                ? t("Linked commits found; the default-branch check is unknown.")
                : merge.kind === "pr_linked"
                  ? t("A pull request is linked; merge state is checked there, not here.")
                  : t("No merge recorded — no pull request and no linked commits.")) +
          (merge.skipped.length
            ? ` ${t("Commit scan skipped:")} ${merge.skipped.join(", ")}.`
            : ""),
      },
      {
        title: t("Deployment"),
        tone: "wait",
        detail:
          deploys.length > 0
            ? t("Deployment references are recorded; no live deployment check exists here.")
            : t("No deployment evidence is attached — this result is not a live-release claim."),
      },
    );
  }

  return (
    <div className="issue-overview grid grid-cols-[minmax(0,1fr)] gap-6" data-testid="work-result">
      <section className="grid gap-1">
        <h2 className="text-label font-medium text-ink-400 m-0">{t("Next action")}</h2>
        <p className="text-cardtitle font-medium text-ink-100 m-0">
          {nextAction({
            detail,
            lane,
            questions: view.questions,
            verdict,
            checksTotal,
            blocked,
            hosted,
            inspectPr: pr !== null && merge.kind !== "merged",
            t,
          })}
        </p>
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-label">
          {progress !== null && (
            <button type="button" className="lnk" aria-label={`${t("Acceptance checks")}: ${progress}`} onClick={openChecklist}>
              {progress}
            </button>
          )}
          <a className="lnk" href={`#${verificationId}`}>{t("Verification")}</a>
          {pr && <Link className="lnk" href={tabHref("pr")}>{pr.label}</Link>}
        </div>
        <dl className="flex flex-wrap items-baseline gap-x-2 gap-y-1 text-label m-0">
          <dt className="text-ink-400">{hosted && !laneView ? t("Assignment") : t("Lane")}</dt>
          <dd className="m-0 text-ink-200">
            {laneView
              ? `${laneView.agent} · ${laneView.state}`
              : hosted
                ? hostedAssignmentText(detail, t)
                : lane === null
                  ? t("Lane state unavailable")
                  : t("No lane yet")}
          </dd>
        </dl>
      </section>

      <div ref={briefRef} className="issue-description issue-reader min-w-0 max-w-[84ch] text-body text-ink-300">
        {children}
      </div>

      <section className="grid grid-cols-[minmax(0,1fr)] gap-2.5">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h2 className="text-cardtitle font-semibold text-ink-100 m-0">
            {t("Result")}
          </h2>
          {latest && (
            <span
              className={`chip ${
                latest.kind === "blocked" || latest.error
                  ? "bg-warn/15 text-warn"
                  : "bg-info/15 text-info"
              }`}
            >
              {latest.kind === "done"
                ? t("Worker reported done")
                : latest.kind === "question"
                  ? t("Worker asked a question")
                  : latest.kind === "blocked"
                    ? t("Worker reported blocked")
                    : `${t("Report")}: ${latest.kind ?? "report"}`}
            </span>
          )}
        </div>
        {latest ? (
          <>
            <p className="text-secondary text-ink-300 m-0">
              {`${latest.agent ?? t("unknown agent")}${
                workerOwn ? ` (${t("the bound agent")})` : ""
              }${latest.at ? ` · ${fmtTime(latest.at)}` : ""}`}
            </p>
            {latest.body && (
              <div className="issue-reader max-w-[84ch] text-secondary text-ink-300">
                <Md text={latest.body} onOpen={onOpen} />
              </div>
            )}
            {latest.error && <p className="text-fail m-0">{latest.error}</p>}
          </>
        ) : latestAssignment && (latestAssignment.text || latestAssignment.deliverable) ? (
          <>
            <p className="text-secondary text-ink-300 m-0">
              <bdi>{isolate(latestAssignment.target ?? detail.owner ?? t("Assigned"))}</bdi>
              {` · ${t(latestAssignment.state)}${
                latestAssignment.updated_at ? ` · ${fmtTime(latestAssignment.updated_at)}` : ""
              }`}
            </p>
            {latestAssignment.text && (
              <div className="issue-reader max-w-[84ch] text-secondary text-ink-300">
                <Md text={latestAssignment.text} onOpen={onOpen} />
              </div>
            )}
            {latestAssignment.deliverable && (latestAssignment.deliverable.ref || latestAssignment.deliverable.title) && (
              <p className="text-secondary text-ink-200 m-0">
                {`${t("Deliverable")}: `}
                {latestAssignment.deliverable.type === "link" && latestAssignment.deliverable.ref ? (
                  <SafeLink className="lnk" href={latestAssignment.deliverable.ref}>
                    {latestAssignment.deliverable.title ?? latestAssignment.deliverable.ref}
                  </SafeLink>
                ) : (
                  <bdi>
                    {isolate(
                      latestAssignment.deliverable.title
                        ? `${latestAssignment.deliverable.title}${
                            latestAssignment.deliverable.ref ? ` (${latestAssignment.deliverable.ref})` : ""
                          }`
                        : latestAssignment.deliverable.ref,
                    )}
                  </bdi>
                )}
              </p>
            )}
          </>
        ) : (
          <p className="text-secondary text-ink-400 m-0">
            {t("No worker report yet. The agent's own statement will appear here when it files one — it is not an operator approval.")}
          </p>
        )}
        {(pr || latest?.pr || deploys.length > 0) && (
          <ul className="issue-output-links m-0 p-0 grid gap-1.5">
            {pr?.href && (
              <li className="num">
                <SafeLink className="lnk" href={pr.href}>
                  {pr.label}
                </SafeLink>
              </li>
            )}
            {pr && !pr.href && (
              <li className="text-ink-400">{`${pr.label} — ${t("this reference has no URL")}`}</li>
            )}
            {latest?.pr && latest.pr !== pr?.href && (
              <li className="num">
                <SafeLink className="lnk" href={latest.pr}>
                  {`${t("Pull request")} (${t("from the worker report")})`}
                </SafeLink>
              </li>
            )}
            {deploys.map((r, i) => (
              <li key={`dep-${i}`} className="num">
                <SafeLink className="lnk" href={r.url}>
                  {r.label ?? r.url ?? r.kind}
                </SafeLink>
              </li>
            ))}
          </ul>
        )}
        <div className="flex flex-wrap items-center gap-2">
          {laneView && (
            <Button variant="primary" disabled={feedbackBlocked} onClick={() => onCompose("instruct")}>
              {t("Leave feedback in the conversation")}
            </Button>
          )}
          <Link className="lnk text-secondary" href={tabHref("evidence")}>
            {artifacts.length ? `${t("Evidence")} (${formatNumber(artifacts.length)})` : t("Open evidence")}
          </Link>
        </div>
        {laneView && composerBlocked(laneView.state) && (
          <p className="text-secondary text-ink-400 m-0">
            {t("Feedback is unavailable for this lane state. Open Conversation to read its history; recovery controls, when available, are on the lane card.")}
          </p>
        )}
        {(latest || (latestAssignment && (latestAssignment.text || latestAssignment.deliverable))) && (
          <p className="text-micro text-ink-500 m-0">
            {codeDelivery
              ? t("A worker report is the agent's own statement — not operator approval, not a merge, not a deployment.")
              : t("A worker report is the agent's own statement — not operator approval.")}
          </p>
        )}
      </section>

      <section className="grid grid-cols-[minmax(0,1fr)] gap-3" aria-labelledby={verificationId}>
        <h2 id={verificationId} tabIndex={-1} className="text-cardtitle font-semibold text-ink-100 m-0">
          {t("Verification")}
        </h2>
        <ul className="issue-checks m-0 p-0 grid gap-2.5">
          {rows.map((row) => (
            <li key={row.title} className="flex gap-2.5 items-start">
              <span
                className={`issue-check-dot mt-1.5 ${TONE_DOT[row.tone]}`}
                aria-hidden="true"
              />
              <div className="min-w-0">
                <strong className="text-ink-200">{row.title}</strong>
                <div className="text-secondary text-ink-400 m-0">{row.detail}</div>
              </div>
            </li>
          ))}
        </ul>
      </section>
    </div>
  );
}
