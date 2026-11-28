import { useEffect } from "react";
import { resources } from "../../lib/resources";
import { useMaybeResource } from "../../lib/useResource";
import { useTranslate } from "../../lib/locale";
import { isolate } from "../decisions/stagedWrite";
import { ageWords, kindSpec, type HomeNeed } from "./needs";
import { cleanPermissionInline } from "./permissionFormat";
import { todoPresentation } from "./railModel";
import { marksFor, useTodoLocal } from "./todoLocal";
import { RailCard } from "./RailCard";

/** The ticket title behind a plan, idea or merge row, once the issue is read. */
function useIssueTitle(need: HomeNeed): string | null {
  const a = need.action;
  const id = a.type === "plan" ? a.epic : a.type === "idea" || a.type === "merge" ? a.issue : null;
  const store = id ? resources.issue(id) : null;
  useEffect(() => {
    if (store) void store.revalidate();
  }, [store]);
  const state = useMaybeResource(store);
  return state?.data && state.data.id === id ? state.data.title : null;
}

/**
 * One unified read-first To do card: status dot, plain headline, meta line,
 * and one quiet cue (`Review ›` or `Inspect ›`). Every card opens its detail
 * drawer before any decision runs; zero inline action controls sit on the card.
 */
export default function TodoCard({
  need,
  doneText,
  onOpen,
}: {
  need: HomeNeed;
  /** What the operator did, once they did it — the card dims. */
  doneText: string | undefined;
  onOpen: (need: HomeNeed, opener: HTMLElement) => void;
}) {
  const t = useTranslate();
  const spec = kindSpec(need);
  const pres = todoPresentation(need);
  const issueTitle = useIssueTitle(need);
  const template = spec.title(need, { issueTitle });
  const title = need.shortTitle ?? template;
  const { sent, sending } = marksFor(useTodoLocal(), need);
  const done = doneText !== undefined;
  const action = need.action;

  const rawSource =
    action.type === "permission"
      ? t("Master")
      : action.type === "staged"
        ? (action.app ?? t("Workspace"))
        : t(pres.sourceLabel);

  const stagedDetail = action.type === "staged" ? need.summary ?? action.askedBy ?? null : null;
  const stagedFooterAsker = action.type === "staged" && need.summary && action.askedBy ? action.askedBy : null;
  const stagedExternal = action.type === "staged" && action.external;

  const batch = action.type === "staged" ? action.batch ?? null : null;
  const batchTone =
    batch?.status === "partial_failed"
      ? ("fail" as const)
      : batch?.status === "running"
        ? ("info" as const)
        : pres.tone;
  const batchKindLabel = batch
    ? batch.status === "partial_failed"
      ? t("Failed · {done}/{total}")
          .replace("{done}", String(batch.completedCount))
          .replace("{total}", String(batch.totalCount))
      : batch.status === "running"
        ? t("Running · {done}/{total}")
            .replace("{done}", String(batch.completedCount))
            .replace("{total}", String(batch.totalCount))
        : t("Batch · {n}").replace("{n}", String(batch.totalCount))
    : t(pres.kindLabel);
  const batchPct =
    batch && batch.totalCount > 0
      ? Math.min(100, Math.round((batch.completedCount / batch.totalCount) * 100))
      : 0;

  const summary = done
    ? doneText
    : batch
      ? (
        <div className="todo-batch-summary">
          {stagedDetail ? (
            <span>
              <bdi>{isolate(stagedDetail)}</bdi>
            </span>
          ) : null}
          {(batch.status === "running" || batch.status === "partial_failed" || batch.completedCount > 0) && (
            <div
              className="dx-batch-prog-track"
              role="progressbar"
              aria-label={t("Batch progress")}
              aria-valuemin={0}
              aria-valuemax={batch.totalCount}
              aria-valuenow={batch.completedCount}
            >
              <div
                className={`dx-batch-prog-fill${batch.failedCount > 0 ? " warn" : ""}`}
                style={{ width: `${batchPct}%` }}
              />
            </div>
          )}
          {batch.status === "pending" && batch.sampleItems.length > 0 && (
            <div className="todo-batch-samples">
              {batch.sampleItems.slice(0, 3).map((item) => (
                <span key={item.itemKey} className="todo-batch-chip">
                  <bdi>{isolate(item.recipient ?? item.label)}</bdi>
                </span>
              ))}
              {batch.totalCount > 3 && (
                <span className="num">+{batch.totalCount - 3}</span>
              )}
            </div>
          )}
        </div>
      )
    : action.type === "staged"
      ? (stagedDetail ? <bdi>{isolate(stagedDetail)}</bdi> : undefined)
      : action.type === "permission"
        ? <span className="font-mono text-micro">{cleanPermissionInline(action.argv)}</span>
        : action.type === "merge"
          ? action.verdict ?? need.summary ?? undefined
          : action.type === "answer"
            ? need.summary ?? action.impact ?? undefined
            : action.type === "plan" || action.type === "idea"
              ? need.summary ?? undefined
              : (need.why && need.why !== rawSource ? need.why : null) ?? need.summary ?? undefined;

  const projectMeta =
    action.type === "staged"
      ? stagedExternal
        ? stagedFooterAsker
          ? <>{t("External")} · <bdi>{isolate(stagedFooterAsker)}</bdi></>
          : t("External")
        : stagedFooterAsker
          ? <bdi>{isolate(stagedFooterAsker)}</bdi>
          : undefined
      : need.project && need.project !== "all" && need.project !== rawSource
        ? need.project
        : undefined;
  const cue = done
    ? t("Done")
    : sending
      ? t("Sending…")
      : sent
        ? t("Sent to Master")
        : batch?.status === "partial_failed"
          ? t("Retry")
          : batch?.status === "running"
            ? t("Progress")
            : t(pres.cueLabel);

  return (
    <li
      className="needrow todo"
      data-need={need.kind}
      data-need-key={need.key}
      data-type={spec.type}
      data-batch-status={batch?.status}
      data-done={done || undefined}
    >
      <RailCard
        className="todo-card"
        bodyClassName="todo-main min-w-0"
        titleClassName="todo-hit"
        metaClassName="todo-meta"
        tone={batchTone}
        statusLabel={batchKindLabel}
        source={action.type === "staged" && action.app ? <bdi>{isolate(rawSource)}</bdi> : rawSource}
        age={ageWords(need.age)}
        done={done}
        disabled={done}
        title={<span className="todo-title" title={title}>{action.type === "staged" ? <bdi>{isolate(title)}</bdi> : title}</span>}
        summary={summary}
        meta={projectMeta}
        external={stagedExternal}
        cue={cue}
        onOpen={(event) => {
          if (done) return;
          onOpen(need, event.currentTarget);
        }}
      />
    </li>
  );
}
