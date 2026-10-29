/**
 * CAD-1941: tasks, linked issues and existing outputs inside the Threads
 * context panel. Opening an output uses the canvas; asking about one cites
 * it on the unsent composer draft.
 */
import type { Agent } from "../../lib/types";
import { useTranslate } from "../../lib/locale";
import Button from "../../ui/Button";
import { KIND_LABEL } from "../canvas/ArtifactCard";
import { makeAskRef } from "../canvas/askRefs";
import { canvasSnapshot, feedCanvas, openArtifact, type ArtifactRef } from "../canvas/canvasStore";
import { setComposerRefs } from "../app-shell/chat/composerStore";
import { IssueRefLink } from "./IssueRefLink";
import {
  assigneeIssues,
  assigneeTasks,
  conversationOutputs,
  type OutputRead,
} from "./threadsContext";

export interface RosterRead {
  status: "loading" | "ready" | "error";
  agents?: Agent[];
  error?: string;
}

/** Bind the canvas to this conversation, then open the recorded card.
 *  The bytes still come from the existing artifact read. */
export function openRecordedArtifact(
  conversation: string,
  composerKey: string | null,
  ref: ArtifactRef,
): void {
  const current = canvasSnapshot();
  if (current.conversation !== conversation || current.composerKey !== composerKey) {
    feedCanvas({
      conversation,
      surface: "home",
      composerKey,
      cards: [],
      working: null,
      turn: null,
    });
  }
  openArtifact(ref);
}

/** Cite the artifact on the composer draft. The draft text stays, and
 *  nothing is sent. */
export function askAboutArtifact(composerKey: string, ref: ArtifactRef): void {
  setComposerRefs(composerKey, [
    makeAskRef({ artifact: ref.id, version: ref.version, title: ref.title, passage: "" }),
  ]);
  if (typeof document !== "undefined") {
    document.querySelector<HTMLTextAreaElement>(".threads-timeline textarea")?.focus();
  }
}

export default function WorkContext({
  assignees,
  roster,
  issueProjects,
  read,
  composerKey,
  conversationId,
}: {
  assignees: string[];
  roster: RosterRead;
  issueProjects: Map<string, string>;
  read: OutputRead;
  composerKey: string | null;
  conversationId: string;
}) {
  const t = useTranslate();
  if (assignees.length === 0) return null;

  const outputs = read.kind === "ready" ? conversationOutputs(read.entries) : [];
  const known = roster.status === "ready" ? roster.agents ?? [] : [];
  const rows = assignees.map((alias) => {
    const agent = known.find((item) => item.alias === alias);
    return {
      alias,
      agent,
      tasks: agent ? assigneeTasks(agent) : [],
      issues: agent ? assigneeIssues(agent) : [],
    };
  });
  const anyTasks = rows.some((row) => row.tasks.length > 0);
  const anyIssues = rows.some((row) => row.issues.length > 0);
  const outputsSettled = read.kind === "ready" || read.kind === "absent";
  const empty = roster.status === "ready"
    && rows.every((row) => row.agent)
    && !anyTasks
    && !anyIssues
    && outputsSettled
    && outputs.length === 0;

  return (
    <section className="threads-work" aria-label={t("Work")} data-work-context>
      {roster.status === "loading" && (
        <p className="threads-note" role="status">{t("Loading roster…")}</p>
      )}
      {roster.status === "error" && (
        <p className="threads-note" role="status">{roster.error ?? t("The roster could not load.")}</p>
      )}
      {empty && (
        <p className="threads-note" data-empty>{t("No recorded tasks or outputs")}</p>
      )}
      {!empty && roster.status === "ready" && anyTasks && (
        <section>
          <h3 className="threads-context-title">{t("Tasks")}</h3>
          <ul className="threads-work-list">
            {rows.flatMap((row) => row.tasks.map((task) => (
              <li key={`${row.alias}:${task.id}`} data-agent={row.alias} data-task={task.id} data-task-state={task.state}>
                <span className="threads-task-line">
                  <span className="threads-task-title">{task.title}</span>
                  <span className="threads-ref">{task.state}</span>
                </span>
              </li>
            )))}
          </ul>
        </section>
      )}
      {!empty && roster.status === "ready" && rows.some((row) => row.agent) && (
        <section>
          <h3 className="threads-context-title">{t("Related tickets")}</h3>
          {anyIssues ? (
            <ul className="threads-work-list">
              {rows.flatMap((row) => row.issues.map((id) => (
                <li key={`${row.alias}:${id}`} data-ticket={id}>
                  <IssueRefLink id={id} project={issueProjects.get(id)} />
                </li>
              )))}
            </ul>
          ) : (
            <p className="threads-note">{t("No linked ticket")}</p>
          )}
        </section>
      )}
      {roster.status === "ready" && rows.some((row) => !row.agent) && (
        <p className="threads-note">{t("Not in the live roster — removed or unavailable")}</p>
      )}
      {!empty && (
        <section data-outputs={read.kind}>
          <h3 className="threads-context-title">{t("Outputs")}</h3>
          {read.kind === "loading" && <p className="threads-note" role="status">{t("Loading…")}</p>}
          {read.kind === "error" && (
            <p className="threads-note" role="status">{t("This conversation could not load:")} {read.error}</p>
          )}
          {read.kind === "withheld" && (
            <p className="threads-note" role="status">{t("Cloud content unavailable")}</p>
          )}
          {outputsSettled && outputs.length === 0 && (
            <p className="threads-note">{t("No outputs")}</p>
          )}
          {outputs.length > 0 && (
            <ul className="threads-work-list">
              {outputs.map((output) => {
                const ref = output.ref;
                if (!ref) {
                  return (
                    <li key={output.key} data-output-missing>
                      <p className="threads-note">{t("Artifact unavailable")}</p>
                    </li>
                  );
                }
                return (
                  <li
                    key={output.key}
                    data-output={ref.id}
                    data-kind={ref.kind}
                    data-version={ref.version}
                    data-output-state={output.state ?? undefined}
                  >
                    <span className="threads-output-line">
                      <span className="arttype">{KIND_LABEL[ref.kind] ?? ref.kind}</span>
                      <span className="threads-output-title">{ref.title}</span>
                      <span className="threads-ref">
                        {ref.kind} · v{ref.version}
                        {output.state ? ` · ${output.state}` : ""}
                      </span>
                    </span>
                    <span className="threads-output-actions">
                      <Button
                        variant="secondary"
                        size="sm"
                        onClick={() => openRecordedArtifact(conversationId, composerKey, ref)}
                      >
                        {t("Open")}
                      </Button>
                      {composerKey && (
                        <Button variant="ghost" size="sm" onClick={() => askAboutArtifact(composerKey, ref)}>
                          {t("Ask about this")}
                        </Button>
                      )}
                    </span>
                  </li>
                );
              })}
            </ul>
          )}
        </section>
      )}
    </section>
  );
}
