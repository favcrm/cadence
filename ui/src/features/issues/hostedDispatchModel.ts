import type { AgentProfile } from "../../lib/generated/board-api";
import type {
  HostedLocalDaemonAgent,
  HostedLocalDaemonRunner,
  HostedLocalHarness,
} from "../../lib/types";

/** Select dispatchable Cloud specialist profiles (excludes system roles like master/reviewer/inbox). */
export function selectCloudSpecialists(
  profiles: readonly AgentProfile[] | undefined,
): AgentProfile[] {
  return (profiles ?? []).filter((profile) => {
    const id = profile.id.trim();
    const alias = profile.alias.trim().toLowerCase();
    const role = (profile.role ?? "").trim().toLowerCase();
    return (
      id.length > 0 &&
      alias.length > 0 &&
      alias !== "master" &&
      alias !== "inbox" &&
      role !== "master" &&
      role !== "reviewer" &&
      role !== "inbox"
    );
  });
}

export type DefaultHostedDispatch =
  | {
      mode: "seat";
      targetAgentId: string;
      runnerId: string;
      reason: "warm_lane" | "seat_fallback";
      spawnDisabledReason: string | null;
    }
  | {
      mode: "spawn";
      runnerId: string;
      provider: HostedLocalHarness;
      repo: string;
      reason: "fresh_spawn";
      spawnDisabledReason: null;
    }
  | {
      mode: "none";
      reason: "no_online_target";
      spawnDisabledReason: string;
    };

export interface OnlineSeatOption {
  runner: HostedLocalDaemonRunner;
  agent: HostedLocalDaemonAgent;
}

export function onlineCurrentRunners(
  runners: readonly HostedLocalDaemonRunner[],
): HostedLocalDaemonRunner[] {
  return runners.filter(
    (runner) => runner.presence === "online" && runner.registration_state === "current",
  );
}

export function onlineCurrentSeats(
  runners: readonly HostedLocalDaemonRunner[],
): OnlineSeatOption[] {
  const out: OnlineSeatOption[] = [];
  for (const runner of onlineCurrentRunners(runners)) {
    for (const agent of runner.agents) {
      if (agent.presence === "online" && agent.registration_state === "current") {
        out.push({ runner, agent });
      }
    }
  }
  return out;
}

export function spawnableRunners(
  runners: readonly HostedLocalDaemonRunner[],
): HostedLocalDaemonRunner[] {
  return onlineCurrentRunners(runners).filter(
    (runner) =>
      runner.spawn_manifest.spawn_allowed &&
      runner.spawn_manifest.active_lanes < runner.spawn_manifest.max_concurrent &&
      runner.spawn_manifest.repos.length > 0 &&
      runner.spawn_manifest.harnesses.length > 0,
  );
}

export function describeSpawnDisabledReason(
  runners: readonly HostedLocalDaemonRunner[],
): string | null {
  if (spawnableRunners(runners).length > 0) return null;
  const live = onlineCurrentRunners(runners);
  if (live.length === 0) return "No online daemon is connected right now.";
  const allowing = live.filter((runner) => runner.spawn_manifest.spawn_allowed);
  if (allowing.length === 0) {
    return "Existing seats only — enable --allow-spawn on the host to unlock isolated worker spawn.";
  }
  const full = allowing.find(
    (runner) => runner.spawn_manifest.active_lanes >= runner.spawn_manifest.max_concurrent,
  );
  if (full) {
    return `Daemon at ${full.spawn_manifest.active_lanes}/${full.spawn_manifest.max_concurrent} lane cap.`;
  }
  return "No allowlisted repository or harness configured on connected daemons.";
}

function preferOmpOrFirst(harnesses: readonly HostedLocalHarness[]): HostedLocalHarness {
  return harnesses.includes("omp") ? "omp" : harnesses[0]!;
}

/**
 * CAD-2746 smart default selector for Hosted Issue Dispatch:
 * 1. If any online current agent is already warm on `issueId`, continue that lane (`mode: "seat"`).
 * 2. Else if an online current runner allows spawn with free slots, pick `mode: "spawn"`.
 * 3. Else fall back to an online current standing seat (`default_assignment_alias` first) with exact disable reason.
 */
export function selectDefaultHostedDispatch(
  runners: readonly HostedLocalDaemonRunner[],
  issueId: string,
): DefaultHostedDispatch {
  const seats = onlineCurrentSeats(runners);
  const spawnDisabledReason = describeSpawnDisabledReason(runners);

  const warm = seats.find(
    ({ agent }) =>
      agent.activity.issue_id === issueId &&
      (agent.activity.status === "busy" || agent.activity.status === "blocked" || agent.activity.status === "idle"),
  );
  if (warm) {
    return {
      mode: "seat",
      targetAgentId: warm.agent.agent_id,
      runnerId: warm.runner.runner_id,
      reason: "warm_lane",
      spawnDisabledReason,
    };
  }

  const spawnable = spawnableRunners(runners)[0];
  if (spawnable) {
    return {
      mode: "spawn",
      runnerId: spawnable.runner_id,
      provider: preferOmpOrFirst(spawnable.spawn_manifest.harnesses),
      repo: spawnable.spawn_manifest.repos[0]!,
      reason: "fresh_spawn",
      spawnDisabledReason: null,
    };
  }

  const fallback =
    seats.find(({ agent }) => agent.default_assignment_alias) ?? seats[0];
  if (fallback) {
    return {
      mode: "seat",
      targetAgentId: fallback.agent.agent_id,
      runnerId: fallback.runner.runner_id,
      reason: "seat_fallback",
      spawnDisabledReason,
    };
  }

  return {
    mode: "none",
    reason: "no_online_target",
    spawnDisabledReason: spawnDisabledReason ?? "No connected daemon seat or spawn host available.",
  };
}
