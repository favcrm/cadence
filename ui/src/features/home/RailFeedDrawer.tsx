import { useEffect, useState } from "react";
import { activityTimeMs } from "../../lib/fmt";
import { useTranslate } from "../../lib/locale";
import type { Agent, AgentTask, DocumentFollowUp } from "../../lib/types";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import AgentAvatar, { agentLook } from "../agents/AgentAvatar";
import { agentThreadHref } from "../agents/AgentBlocks";
import { useAssistantActor } from "../assistant/AssistantIdentity";
import { displayName } from "../assistant/identity";
import DrawerShell from "../app-shell/shared/DrawerShell";
import { ageText, failureReason, isUncertain, isolate, outcomeOf, reasonText, titleOf, type StagedWrite } from "../decisions/stagedWrite";
import { agentWorkHref, type AgentUpdate } from "./agentUpdateModel";
import { ageLabel, ageWords, metaLine, updateTitle, type HomeNeed } from "./needs";
import { decidedWriteStatusLabel } from "./railModel";

export type RailFeedTarget =
  | {
      kind: "agent";
      agent: Agent;
      updates: readonly AgentUpdate[];
      tasks: readonly AgentTask[];
      stateLabel: string;
    }
  | { kind: "document"; doc: DocumentFollowUp & { outcome: "approved" | "delivered" } }
  | { kind: "decided_write"; write: StagedWrite }
  | { kind: "info_need"; need: HomeNeed };

/**
 * Read-only / talk-to-agent detail drawer for org Feed items.
 * Never renders approval mutations (`Approve`, `Approve & send`, `Request changes`).
 */
export default function RailFeedDrawer({
  target,
  readOnly,
  onAskAgent,
  onAskNeed,
  onOpenIssue,
  onClose,
}: {
  target: RailFeedTarget | null;
  readOnly: boolean;
  onAskAgent?: (agent: Agent, update?: AgentUpdate) => void;
  onAskNeed?: (need: HomeNeed) => void;
  onOpenIssue?: (id: string) => void;
  onClose: (restoreFocus?: boolean) => void;
}) {
  const t = useTranslate();
  const actor = useAssistantActor();
  const assistantName = displayName(actor.read, actor.scope, t);
  const [showRaw, setShowRaw] = useState(false);

  useEffect(() => {
    setShowRaw(false);
  }, [target]);

  if (!target) return null;

  if (target.kind === "agent") {
    const { agent, updates, tasks, stateLabel } = target;
    const isWorking = agent.running > 0;
    const look = typeof agent.look === "string" ? agent.look : undefined;
    const context =
      tasks.find((task) => task.title || task.job_title)?.title ??
      tasks.find((task) => task.job_title)?.job_title ??
      null;

    return (
      <DrawerShell
        kind="feed-detail"
        label={t("Feed")}
        kicker={t("Feed")}
        scrim="board"
        modal
        trapFocus
        avatar={
          <AgentAvatar
            slug={agentLook(agent.alias, look ? { look } : agent.params)}
            look={look}
            size={36}
            still={!isWorking}
            active={isWorking}
          />
        }
        title={agent.alias}
        subtitle={context ?? undefined}
        pills={
          <span className="s-status-pill" data-tone={isWorking ? "ok" : agent.fenced ? "warn" : undefined}>
            {t(stateLabel)}
          </span>
        }
        secondary={
          onAskAgent && !readOnly ? (
            <Button
              size="sm"
              variant="secondary"
              onClick={() => {
                onAskAgent(agent, updates[0]);
                onClose(false);
              }}
            >
              {t("Ask {name}").replace("{name}", () => assistantName)} ↗
            </Button>
          ) : undefined
        }
        primary={
          <Link href={agentThreadHref(agent.alias)} className="btn btn-sm btn-primary" onClick={() => onClose(false)}>
            {t("Talk to {alias}").replace("{alias}", () => agent.alias)} ↗
          </Link>
        }
        tabs={[
          {
            id: "activity",
            label: t("Activity"),
            panel: (
              <div className="dx-panel">
                {tasks.length > 0 && (
                  <div className="dx-kv-card">
                    <div className="slabel">{t("Active assigned work")}</div>
                    <ul className="space-y-2 mt-1">
                      {tasks.map((task) => (
                        <li key={task.task} className="text-label border-t border-ink-800 pt-2 first:border-t-0 first:pt-0">
                          <div className="font-medium text-ink-100">{task.title ?? task.task}</div>
                          <div className="text-micro text-ink-400">
                            {task.task_state}
                            {task.job_title ? ` · ${task.job_title}` : ""}
                          </div>
                        </li>
                      ))}
                    </ul>
                  </div>
                )}

                {updates.length > 0 ? (
                  <div className="dx-kv-card">
                    <div className="slabel">{t("Recent updates")}</div>
                    <ul className="space-y-2.5 mt-1">
                      {updates.map((u) => {
                        const stamp = activityTimeMs(u.at);
                        return (
                          <li key={u.key} className="border-t border-ink-800 pt-2 first:border-t-0 first:pt-0 space-y-1">
                            <div className="flex items-center justify-between gap-2 text-micro text-ink-400">
                              <span className="font-medium text-ink-200">{t(u.label)}</span>
                              <span className="flex items-center gap-2 num">
                                {u.issue && onOpenIssue && (
                                  <button type="button" className="lnk num" onClick={() => { onOpenIssue(u.issue!); onClose(false); }}>
                                    {u.issue}
                                  </button>
                                )}
                                {stamp ? <span>{ageLabel((Date.now() - stamp) / 1000)} {t("ago")}</span> : null}
                              </span>
                            </div>
                            {u.text && <p className="text-label text-ink-300 whitespace-pre-wrap break-words">{u.text}</p>}
                          </li>
                        );
                      })}
                    </ul>
                  </div>
                ) : (
                  <p className="text-label text-ink-400">{t("No recent progress notes.")}</p>
                )}

                <div className="flex items-center justify-between gap-2 text-micro text-ink-500">
                  <span>{t("Work receipt only — private chats are never exposed.")}</span>
                  <Link href={agentWorkHref(agent.alias)} className="lnk shrink-0" onClick={() => onClose(false)}>
                    {t("Agent profile")}
                  </Link>
                </div>
              </div>
            ),
          },
        ]}
        onClose={() => onClose()}
      />
    );
  }

  if (target.kind === "document") {
    const doc = target.doc;
    const href = `/wiki?file=${encodeURIComponent(doc.id)}&version=${doc.version}`;
    const outcome = doc.outcome === "delivered" ? t("Delivered") : t("Approved");
    return (
      <DrawerShell
        kind="feed-detail"
        label={t("Feed")}
        kicker={t("Feed")}
        scrim="board"
        modal
        trapFocus
        title={doc.title}
        subtitle={`${doc.project ? `${doc.project} · ` : ""}v${doc.version}`}
        pills={<span className="s-status-pill" data-tone="ok">{outcome}</span>}
        secondary={<Button size="sm" variant="ghost" onClick={() => onClose()}>{t("Close")}</Button>}
        primary={<Link href={href} className="btn btn-sm btn-primary" onClick={() => onClose(false)}>{t("Open in Wiki")} ↗</Link>}
        tabs={[{
          id: "detail",
          label: t("Details"),
          panel: (
            <div className="dx-panel">
              <dl className="dx-kv-card">
                <div className="dx-kv-line"><dt>{t("Version")}</dt><dd className="num">v{doc.version}</dd></div>
                {doc.project && <div className="dx-kv-line"><dt>{t("Project")}</dt><dd>{doc.project}</dd></div>}
                <div className="dx-kv-line"><dt>{t("Status")}</dt><dd>{outcome}</dd></div>
              </dl>
              <p className="text-micro text-ink-500">{t("Read-only activity")}</p>
            </div>
          ),
        }]}
        onClose={onClose}
      />
    );
  }

  if (target.kind === "decided_write") {
    const write = target.write;
    const outcome = outcomeOf(write);
    const when = ageText(write.decidedAt ?? write.createdAt, t);
    const statusLabel = decidedWriteStatusLabel(write, outcome, t);

    return (
      <DrawerShell
        kind="feed-detail"
        label={t("Feed")}
        kicker={t("Feed")}
        scrim="board"
        modal
        trapFocus
        title={<bdi>{isolate(titleOf(write, t))}</bdi>}
        subtitle={`${statusLabel} · ${when}`}
        pills={<span className="s-status-pill" data-tone={outcome === "failed" || outcome === "unconfirmed" ? "warn" : "ok"}>{statusLabel}</span>}
        note={t("Read-only activity")}
        primary={<Button size="sm" variant="ghost" onClick={() => onClose()}>{t("Close")}</Button>}
        tabs={[{
          id: "detail",
          label: t("Details"),
          panel: (
            <div className="dx-panel">
              {(outcome === "failed" || (outcome === "unconfirmed" && write.connected !== null && isUncertain(write))) && (
                <p className="text-label text-ink-300">{reasonText(failureReason(write), t)}</p>
              )}
              {outcome === "failed" && (
                <div className="dx-why">
                  <button type="button" className="lnk dx-rawbtn" aria-expanded={showRaw} onClick={() => setShowRaw((r) => !r)}>
                    {showRaw ? t("Hide raw") : t("Show raw")}
                  </button>
                  {showRaw && (
                    <pre className="rv-tech dx-raw" tabIndex={0}>
                      {write.result?.code ? `${t("code")}: ${write.result.code}\n\n` : ""}
                      {write.raw}
                    </pre>
                  )}
                </div>
              )}
            </div>
          ),
        }]}
        onClose={onClose}
      />
    );
  }

  const need = target.need;
  return (
    <DrawerShell
      kind="feed-detail"
      label={t("Feed")}
      kicker={t("Feed")}
      scrim="board"
      modal
      trapFocus
      title={updateTitle(need)}
      subtitle={metaLine(need)}
      note={t("Read-only activity")}
      secondary={need.issue && onOpenIssue ? (
        <Button size="sm" variant="secondary" onClick={() => { onOpenIssue(need.issue!); onClose(false); }}>
          {need.issue} ↗
        </Button>
      ) : undefined}
      primary={onAskNeed && !readOnly ? (
        <Button size="sm" variant="primary" onClick={() => { onAskNeed(need); onClose(false); }}>
          {t("Ask {name}").replace("{name}", () => assistantName)} ↗
        </Button>
      ) : (
        <Button size="sm" variant="ghost" onClick={() => onClose()}>{t("Close")}</Button>
      )}
      tabs={[{
        id: "detail",
        label: t("Details"),
        panel: (
          <div className="dx-panel">
            <dl className="dx-kv-card">
              {need.project && need.project !== "all" && <div className="dx-kv-line"><dt>{t("Project")}</dt><dd>{need.project}</dd></div>}
              {need.issue && <div className="dx-kv-line"><dt>{t("Issue")}</dt><dd className="num">{need.issue}</dd></div>}
              <div className="dx-kv-line"><dt>{t("Observed")}</dt><dd className="num">{ageWords(need.age)}</dd></div>
            </dl>
          </div>
        ),
      }]}
      onClose={onClose}
    />
  );
}
