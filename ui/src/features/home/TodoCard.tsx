import { useEffect } from "react";
import { resources } from "../../lib/resources";
import { useMaybeResource } from "../../lib/useResource";
import { useTranslate } from "../../lib/locale";
import { isolate } from "../decisions/stagedWrite";
import { ageWords, kindSpec, metaLine, type HomeNeed } from "./needs";
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
      : t(pres.sourceLabel);

  const summary = done
    ? doneText
    : action.type === "staged"
      ? <bdi>{isolate(metaLine(need))}</bdi>
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
    action.type !== "staged" && need.project && need.project !== "all" && need.project !== rawSource
      ? need.project
      : undefined;
  const cue = done
    ? t("Done")
    : sending
      ? t("Sending…")
      : sent
        ? t("Sent to Master")
        : t(pres.cueLabel);

  return (
    <li
      className="needrow todo"
      data-need={need.kind}
      data-need-key={need.key}
      data-type={spec.type}
      data-done={done || undefined}
    >
      <RailCard
        className="todo-card"
        bodyClassName="todo-main min-w-0"
        titleClassName="todo-hit"
        metaClassName="todo-meta"
        tone={pres.tone}
        statusLabel={t(pres.kindLabel)}
        source={action.type === "staged" ? <bdi>{isolate(rawSource)}</bdi> : rawSource}
        age={ageWords(need.age)}
        done={done}
        disabled={done}
        title={<span className="todo-title" title={title}>{action.type === "staged" ? <bdi>{isolate(title)}</bdi> : title}</span>}
        summary={summary}
        meta={projectMeta}
        cue={cue}
        onOpen={(event) => {
          if (done) return;
          onOpen(need, event.currentTarget);
        }}
      />
    </li>
  );
}
