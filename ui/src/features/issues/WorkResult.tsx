import { useContext, type ReactNode } from "react";
import SafeLink from "../../ui/SafeLink";
import Button from "../../ui/Button";
import Md from "../../ui/Md";
import { fmtTime } from "../../lib/fmt";
import { useLocale } from "../../lib/locale";
import type { IssueDetail, TaskReport } from "../../lib/types";
import { stageLabel } from "../projects/work";
import { composerBlocked, type LanePayload } from "./lane";
import { WriteGate } from "../auth/WriteGate";
import { outcomeOf, type StagedWrite } from "../decisions/stagedWrite";
import {
  deployRefs,
  hasCodeDelivery,
  hostedAssignmentText,
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
  t: (s: string) => string;
}): string {
  if (detail.status === "done" || detail.status === "dropped")
    return t("The issue is closed; nothing is pending here.");
  if (hosted && !lane?.lane) {
    if (questions.length > 0)
      return `${t("Answer the open question")} — ${questions[0].body?.split("\n")[0] ?? questions[0].name}`;
    if (verdict?.verdict === "revise")
      return t("The independent review asked for a revision.");
    if (detail.status === "review")
      return t("Review the deliverable and recorded checks below.");
    const hasAssignment =
      Boolean(detail.owner) ||
      Boolean(detail.agent_run?.agent) ||
      Boolean(detail.local_assignment?.available && detail.local_assignment.assignments.length > 0);
    if (hasAssignment)
      return t("Assigned work is active — check Activity or Evidence for updates.");
    return t("Assign an owner or update acceptance checks to move this item forward.");
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
  codeDelivery = hasCodeDelivery(detail),
  stagedActions,
  tabHref,
  onCompose,
  onKickoff,
  onOpen,
}: {
  detail: IssueDetail;
  lane: LanePayload | null;
  checksDone: number;
  checksTotal: number;
  blocked: string | null;
  hosted?: boolean;
  codeDelivery?: boolean;
  stagedActions?: readonly StagedWrite[];
  tabHref: (tab: IssueTab) => string;
  onCompose: (mode: "ask" | "instruct") => void;
  onKickoff: () => void;
  onOpen: (id: string) => void;
}) {
  const { t, formatNumber } = useLocale();
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
  const assignmentRows = detail.local_assignment?.available
    ? detail.local_assignment.assignments
    : [];
  const latestAssignment =
    assignmentRows.length > 0 ? assignmentRows[assignmentRows.length - 1] : null;
  const approvalHeads = detail.approvals?.heads ?? [];
  const inForceHeads = approvalHeads.filter((h) => h.state === "in-force");
  const stagedMatches = matchingStagedActions(detail.id, stagedActions);
  const linkedRows = shownLinks(detail.links, detail.relates);

  const rows: { title: string; detail: ReactNode; tone: keyof typeof TONE_DOT }[] = [
    {
      title: t("Acceptance checks"),
      tone: checksTotal === 0 || checksDone < checksTotal ? "wait" : "done",
      detail:
        checksTotal === 0
          ? t("No acceptance checklist on the issue.")
          : `${formatNumber(checksDone)} ${t("of")} ${formatNumber(checksTotal)} ${t("checked")}`,
    },
    {
      title: t("Worker report"),
      tone: latest || latestAssignment?.text || latestAssignment?.deliverable ? "now" : "wait",
      detail: latest
        ? `${latest.agent ?? t("unknown agent")} · ${latest.kind ?? "report"}${
            latest.at ? ` · ${fmtTime(latest.at)}` : ""
          }`
        : latestAssignment && (latestAssignment.text || latestAssignment.deliverable)
          ? `${latestAssignment.target ?? detail.owner ?? t("Assigned")} · ${t(latestAssignment.state)}${
              latestAssignment.updated_at ? ` · ${fmtTime(latestAssignment.updated_at)}` : ""
            }`
          : t("Nothing reported yet — no completion claim exists."),
    },
  ];

  if (
    approvalHeads.length > 0 ||
    stagedMatches.length > 0 ||
    (detail.approvals !== undefined && detail.approvals !== null && !detail.approvals.available) ||
    !codeDelivery
  ) {
    const pendingStaged = stagedMatches.some((s) => outcomeOf(s) === "pending" || outcomeOf(s) === "failed");
    const appliedStaged = stagedMatches.some((s) => outcomeOf(s) === "applied");
    rows.push({
      title: t("Approvals"),
      tone:
        (detail.approvals && !detail.approvals.available) || pendingStaged
          ? "warn"
          : inForceHeads.length > 0 || appliedStaged
            ? "done"
            : "wait",
      detail:
        detail.approvals && !detail.approvals.available && stagedMatches.length === 0 ? (
          t("Approval records could not be read.")
        ) : approvalHeads.length > 0 || stagedMatches.length > 0 ? (
          <ul className="m-0 p-0 list-none grid gap-1">
            {approvalHeads.map((head) => (
              <li
                key={`${head.repo}#${head.pr}@${head.head_sha}`}
                className="num text-secondary"
              >
                <span>{`${head.repo} #${head.pr} @ ${head.head_sha.slice(0, 12)}`}</span>
                {" · "}
                <span>{t(head.state)}</span>
                {head.source ? ` · ${head.source}` : ""}
                {head.action ? ` (${head.action})` : ""}
                {head.recorded_at ? ` · ${fmtTime(head.recorded_at)}` : ""}
              </li>
            ))}
            {stagedMatches.map((item) => (
              <li key={item.id} className="text-secondary">
                <span>{item.title}</span>
                {" · "}
                <span className="num">{t(outcomeOf(item))}</span>
                {item.decidedAt || item.createdAt
                  ? ` · ${fmtTime(new Date(item.decidedAt ?? item.createdAt).toISOString())}`
                  : ""}
              </li>
            ))}
          </ul>
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
    <section className="grid gap-4" data-testid="work-result">
      <div className="card p-4 grid gap-3">
        <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-sm m-0">
          <dt className="text-ink-500">{t("Status")}</dt>
          <dd className="min-w-0 m-0">
            {t(detail.status)}
            {detail.work?.stage && ` · ${stageLabel(detail.work.stage, t)}`}
          </dd>
          <dt className="text-ink-500">
            {hosted && !laneView ? t("Assignment") : t("Lane")}
          </dt>
          <dd className="num min-w-0 m-0">
            {laneView
              ? `${laneView.agent} · ${laneView.state}`
              : hosted
                ? hostedAssignmentText(detail, t)
                : lane === null
                  ? t("Lane state unavailable")
                  : t("No lane yet")}
          </dd>
          <dt className="text-ink-500">{t("Next action")}</dt>
          <dd className="min-w-0 m-0">
            {nextAction({
              detail,
              lane,
              questions: view.questions,
              verdict,
              checksTotal,
              blocked,
              hosted,
              t,
            })}
          </dd>
        </dl>
      </div>

      <article className="card p-4 grid gap-3">
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
              {`${latestAssignment.target ?? detail.owner ?? t("Assigned")} · ${t(latestAssignment.state)}${
                latestAssignment.updated_at ? ` · ${fmtTime(latestAssignment.updated_at)}` : ""
              }`}
            </p>
            {latestAssignment.text && (
              <div className="issue-reader max-w-[84ch] text-secondary text-ink-300">
                <Md text={latestAssignment.text} onOpen={onOpen} />
              </div>
            )}
            {latestAssignment.deliverable?.title && (
              <p className="text-secondary text-ink-200 m-0">
                {`${t("Deliverable")}: ${latestAssignment.deliverable.title}${
                  latestAssignment.deliverable.ref ? ` (${latestAssignment.deliverable.ref})` : ""
                }`}
              </p>
            )}
          </>
        ) : (
          <p className="text-secondary text-ink-400 m-0">
            {t("No worker report yet. The agent's own statement will appear here when it files one — it is not an operator approval.")}
          </p>
        )}
        {(pr || latest?.pr || artifacts.length > 0 || deploys.length > 0) && (
          <ul className="issue-output-links m-0 p-0 grid gap-1.5">
            {pr?.href && (
              <li className="num truncate">
                <SafeLink className="lnk" href={pr.href}>
                  {pr.label}
                </SafeLink>
              </li>
            )}
            {pr && !pr.href && (
              <li className="text-ink-400">{`${pr.label} — ${t("this reference has no URL")}`}</li>
            )}
            {latest?.pr && latest.pr !== pr?.href && (
              <li className="num truncate">
                <SafeLink className="lnk" href={latest.pr}>
                  {`${t("Pull request")} (${t("from the worker report")})`}
                </SafeLink>
              </li>
            )}
            {deploys.map((r, i) => (
              <li key={`dep-${i}`} className="num truncate">
                <SafeLink className="lnk" href={r.url}>
                  {r.label ?? r.url ?? r.kind}
                </SafeLink>
              </li>
            ))}
            {artifacts.length > 0 && (
              <li>
                <Button size="sm" variant="ghost" href={tabHref("evidence")}>
                  {`${t("Evidence")} (${formatNumber(artifacts.length)})`}
                </Button>
              </li>
            )}
          </ul>
        )}
        <div className="flex flex-wrap gap-2">
          {laneView ? (
            <Button variant="primary" disabled={feedbackBlocked} onClick={() => onCompose("instruct")}>
              {t("Leave feedback in the conversation")}
            </Button>
          ) : (
            !hosted &&
            !blocked && (
              <Button variant="primary" onClick={onKickoff}>
                {t("Kick off")}
              </Button>
            )
          )}
          {codeDelivery && <Button href={tabHref("pr")}>{t("Pull request")}</Button>}
          <Button variant="ghost" href={tabHref("evidence")}>
            {t("Open evidence")}
          </Button>
        </div>
        {laneView && composerBlocked(laneView.state) && (
          <p className="text-secondary text-ink-400 m-0">
            {t("Feedback is unavailable for this lane state. Open Conversation to read its history; recovery controls, when available, are on the lane card.")}
          </p>
        )}
        <p className="text-micro text-ink-500 m-0">
          {codeDelivery
            ? t("A worker report is the agent's own statement — not operator approval, not a merge, not a deployment.")
            : t("A worker report is the agent's own statement — not operator approval.")}
        </p>
      </article>

      <section className="card p-4 grid gap-3" aria-label={t("Verification")}>
        <h2 className="text-cardtitle font-semibold text-ink-100 m-0">
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
    </section>
  );
}
