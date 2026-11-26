import { useEffect, useRef, type RefObject } from "react";
import { laneFollowsStream } from "../features/issues/model";
import { noteThreadsChanged } from "../features/threads/threadsClient";
import { agentRowForEvent } from "./agentOn";
import { LiveUpdates, patchRows } from "./liveUpdates";
import { noteMasterChanged } from "./masterChanged";
import { cache, resources, type MyWorkList } from "./resources";
import { buildChanged, serverBuild, subscribeSse, UI_BUILD } from "./sse";
import type { Agent, AgentsPayload, IssueCard } from "./types";

export function useBoardLiveUpdates(
  reconcile: () => void,
  invalidatePersonal: () => void,
  overviewWantedRef: RefObject<boolean>,
  mergePersonalCard: (previous: MyWorkList, card: IssueCard) => MyWorkList,
  setPartialUpdates: (partial: boolean) => void,
  setStaleBuild: (build: string | null) => void,
): RefObject<{ healthy: () => boolean } | null> {
  const liveUpdates = useRef<LiveUpdates | null>(null);
  useEffect(() => {
    const updates = new LiveUpdates({
      resync: reconcile,
      onDegraded: setPartialUpdates,
      invalidate: (key) => {
        // Main refetches the lane store when a frame names issue, agents,
        // or jobs. Entity patches queue that through LiveUpdates; legacy
        // resource names still have to.
        if (laneFollowsStream([key])) cache.invalidate("lane");
        if (key === "issues" || key === "issue" || key === "agents") invalidatePersonal();
        if (key === "threads") noteThreadsChanged();
        if (key === "master") noteMasterChanged();
        if (key === "overview" && !overviewWantedRef.current) {
          resources.overview.markInvalid();
        } else cache.invalidate(key);
      },
      patch: (event, data) => {
        if (event.type === "issue") {
          if (typeof data.id !== "string" || !["upsert", "delete"].includes(String(data.op)) ||
              (data.op === "upsert" && (!data.issue || typeof data.issue !== "object"))) {
            reconcile(); return;
          }
          if (resources.issues.get().data === null) {
            void resources.issues.invalidate();
          } else resources.issues.mutate((rows) => patchRows(rows, data.id as string, data.op, data.issue as IssueCard, (row) => row.id));
          resources.myWork.mutate((body) => data.op === "delete"
            ? { ...body, issues: body.issues.filter((row) => row.id !== data.id) }
            : mergePersonalCard(body, data.issue as IssueCard));
          invalidatePersonal();
        } else if (event.type === "agent") {
          if (typeof data.id !== "string" || !["upsert", "delete"].includes(String(data.op)) ||
              (data.op === "upsert" && (!data.agent || typeof data.agent !== "object"))) {
            reconcile(); return;
          }
          if (resources.agents.get().data === null) {
            void resources.agents.invalidate();
          } else resources.agents.mutate((value) => ({ ...value,
            agents: patchRows(
              value.agents,
              data.id as string,
              data.op,
              // A delete tombstone is { op, id } with no row; only an upsert
              // carries a row worth normalizing.
              agentRowForEvent(data.op as "upsert" | "delete", data.agent as Agent | undefined) as Agent,
              (row) => row.alias,
            ),
          }));
        } else {
          if (!data.by_issue || !data.daemon || !("totals" in data)) { reconcile(); return; }
          resources.agents.mutate((value) => ({ ...value,
            daemon: data.daemon as AgentsPayload["daemon"],
            totals: data.totals as AgentsPayload["totals"],
            by_issue: data.by_issue as AgentsPayload["by_issue"],
            profiles: Array.isArray(data.profiles) ? (data.profiles as AgentsPayload["profiles"]) : value.profiles,
            feed_items: Array.isArray(data.feed_items) ? (data.feed_items as AgentsPayload["feed_items"]) : value.feed_items,
          }));
          // Issue-page lane stores depend on runtime state, including
          // changes which leave their issue card unchanged.
          // LiveUpdates batches the dependent lane invalidation.
        }
      },
    });
    liveUpdates.current = updates;
    const sub = subscribeSse({
      url: "/api/stream?entities=1&accept_degraded=1",
      events: ["hello", "heartbeat", "issues", "agents", "jobs", "monitoring", "issue", "agent", "agent_meta", "plan", "aggregates", "threads", "master"],
      onEvent: updates.event,
      onOpen: updates.opened,
      onError: updates.failed,
      onBuild: (server) => setStaleBuild(buildChanged(server, UI_BUILD) ? server : null),
      probeBuild: serverBuild,
    });
    return () => {
      updates.close();
      sub.close();
      if (liveUpdates.current === updates) liveUpdates.current = null;
    };
  }, [reconcile, invalidatePersonal, overviewWantedRef, mergePersonalCard, setPartialUpdates, setStaleBuild]);

  return liveUpdates;
}
