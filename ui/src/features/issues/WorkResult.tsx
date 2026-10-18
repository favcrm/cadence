import type { ReactNode } from "react";
import SafeLink from "../../ui/SafeLink";
import Button from "../../ui/Button";
import Md from "../../ui/Md";
import { fmtTime } from "../../lib/fmt";
import { useLocale } from "../../lib/locale";
import type { IssueDetail, TaskReport } from "../../lib/types";
import { stageLabel } from "../projects/work";
import type { LanePayload } from "./lane";
import {
  deployRefs,
  mergeState,
  prRef,
  reportView,
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
  t,
}: {
  detail: IssueDetail;
  /** Null while the lane resource is loading or failed — not "no lane". */
  lane: LanePayload | null;
  questions: TaskReport[];
  verdict: TaskReport | null;
  checksTotal: number;
  blocked: string | null;
  t: (s: string) => string;
}): string {
  if (detail.status === "done" || detail.status === "dropped")
    return t("The issue is closed; nothing is pending here.");
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
 * CAD-1574 overview: objective, stage, next action, real output links
 * and the existing checks/reviews in one place. Worker reports,
 * revision-bound verdicts, merge and deployment stay distinct; anything
 * the board cannot verify is said plainly instead of inferred.
 */
export function WorkResult({
  detail,
  lane,
  checksDone,
  checksTotal,
  blocked,
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
  tabHref: (tab: IssueTab) => string;
  onCompose: (mode: "ask" | "instruct") => void;
  onKickoff: () => void;
  onOpen: (id: string) => void;
}) {
  const { t, formatNumber } = useLocale();
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
      tone: latest ? "now" : "wait",
      detail: latest
        ? `${latest.agent ?? t("unknown agent")} · ${latest.kind ?? "report"}${
            latest.at ? ` · ${fmtTime(latest.at)}` : ""
          }`
        : t("Nothing reported yet — no completion claim exists."),
    },
    {
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
              {t("Newer work landed after this review — it may not cover the latest revision.")}
            </p>
          )}
        </>
      ),
    },
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
  ];

  return (
    <section className="grid gap-4" data-testid="work-result">
      <div className="card p-4 grid gap-3">
        <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-sm m-0">
          <dt className="text-ink-500">{t("Status")}</dt>
          <dd className="min-w-0 m-0">
            {t(detail.status)}
            {detail.work?.stage && ` · ${stageLabel(detail.work.stage, t)}`}
          </dd>
          <dt className="text-ink-500">{t("Lane")}</dt>
          <dd className="num min-w-0 m-0">
            {laneView
              ? `${laneView.agent} · ${laneView.state}`
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
            {latest?.pr && !pr?.href && (
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
            <Button variant="primary" onClick={() => onCompose("instruct")}>
              {t("Leave feedback in the conversation")}
            </Button>
          ) : (
            !blocked && (
              <Button variant="primary" onClick={onKickoff}>
                {t("Kick off")}
              </Button>
            )
          )}
          <Button href={tabHref("pr")}>{t("Pull request")}</Button>
          <Button variant="ghost" href={tabHref("evidence")}>
            {t("Open evidence")}
          </Button>
        </div>
        <p className="text-micro text-ink-500 m-0">
          {t("A worker report is the agent's own statement — not operator approval, not a merge, not a deployment.")}
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
