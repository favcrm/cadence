export {};
import {
  selectCloudSpecialists,
  selectDefaultHostedDispatch,
} from "../src/features/issues/hostedDispatchModel";
import type { HostedLocalDaemonRunner } from "../src/lib/types";

function assert(cond: boolean, why: string): void {
  if (!cond) throw new Error(why);
}

const runnerBase = (
  over: Partial<HostedLocalDaemonRunner> = {},
): HostedLocalDaemonRunner => ({
  runner_id: "lri_00000000-0000-4000-8000-000000000001",
  host_label: "macbook-pro",
  platform: "darwin",
  owner_subject_id: "usr_owner",
  presence: "online",
  registration_state: "current",
  spawn_manifest: {
    spawn_allowed: true,
    repos: ["fav-limited/cadence"],
    harnesses: ["omp", "pi"],
    max_concurrent: 2,
    active_lanes: 0,
  },
  agents: [
    {
      agent_id: "lag_00000000-0000-4000-8000-000000000001",
      label: "default-pi",
      adapter: "pi/rpc",
      delivery: "turn",
      client_agent_id: "seat-default",
      default_assignment_alias: true,
      registration_state: "current",
      presence: "online",
      activity: { status: "idle" },
    },
  ],
  ...over,
});

// 1. Warm existing lane wins even when fresh spawn is available.
{
  const warmRunner = runnerBase({
    agents: [
      {
        agent_id: "lag_00000000-0000-4000-8000-000000000099",
        label: "warm-omp",
        adapter: "omp/rpc",
        delivery: "turn",
        client_agent_id: "seat-warm",
        default_assignment_alias: false,
        registration_state: "current",
        presence: "online",
        activity: { status: "busy", issue_id: "CAD-2746", summary: "Running tests" },
      },
    ],
  });
  const picked = selectDefaultHostedDispatch([warmRunner], "CAD-2746");
  assert(
    picked.mode === "seat" &&
      picked.reason === "warm_lane" &&
      picked.targetAgentId === "lag_00000000-0000-4000-8000-000000000099",
    "warm existing lane on same issue must default to seat continuation",
  );
}

// 2. Fresh issue + free spawn capacity defaults to spawn with omp preference.
{
  const freshRunner = runnerBase({
    spawn_manifest: {
      spawn_allowed: true,
      repos: ["fav-limited/cadence"],
      harnesses: ["claude", "omp"],
      max_concurrent: 3,
      active_lanes: 1,
    },
  });
  const picked = selectDefaultHostedDispatch([freshRunner], "CAD-2900");
  assert(
    picked.mode === "spawn" &&
      picked.reason === "fresh_spawn" &&
      picked.provider === "omp" &&
      picked.repo === "fav-limited/cadence" &&
      picked.spawnDisabledReason === null,
    "fresh issue with capacity picks spawn and prefers omp",
  );
}

// 3. Full lane capacity disables spawn with exact cap reason and falls back to default seat.
{
  const fullRunner = runnerBase({
    spawn_manifest: {
      spawn_allowed: true,
      repos: ["fav-limited/cadence"],
      harnesses: ["omp"],
      max_concurrent: 2,
      active_lanes: 2,
    },
  });
  const picked = selectDefaultHostedDispatch([fullRunner], "CAD-2900");
  assert(
    picked.mode === "seat" &&
      picked.reason === "seat_fallback" &&
      picked.spawnDisabledReason === "Daemon at 2/2 lane cap.",
    `expected cap fallback reason, got ${JSON.stringify(picked)}`,
  );
}

// 4. Spawn disabled falls back to seat with --allow-spawn hint.
{
  const lockedRunner = runnerBase({
    spawn_manifest: {
      spawn_allowed: false,
      repos: [],
      harnesses: [],
      max_concurrent: 0,
      active_lanes: 0,
    },
  });
  const picked = selectDefaultHostedDispatch([lockedRunner], "CAD-2900");
  assert(
    picked.mode === "seat" &&
      picked.reason === "seat_fallback" &&
      (picked.spawnDisabledReason ?? "").includes("--allow-spawn"),
    "disabled spawn must explain --allow-spawn requirement",
  );
}

// 5. Cloud specialists filter out master, reviewer, and inbox system profiles.
{
  const specialists = selectCloudSpecialists([
    {
      id: "ap_master",
      alias: "master",
      catalogue: null,
      job: null,
      latest_claim: null,
      look: null,
      model: "inherit",
      role: "master",
      runtimes: [],
      team_role: "master",
    },
    {
      id: "ap_rev",
      alias: "review-bot",
      catalogue: null,
      job: null,
      latest_claim: null,
      look: null,
      model: "inherit",
      role: "reviewer",
      runtimes: [],
      team_role: "reviewer",
    },
    {
      id: "ap_builder",
      alias: "Builder",
      catalogue: "builder",
      job: null,
      latest_claim: null,
      look: null,
      model: "claude-sonnet-4",
      role: "worker",
      runtimes: [],
      team_role: "builder",
    },
  ]);
  assert(
    specialists.length === 1 && specialists[0]!.id === "ap_builder",
    `expected only ap_builder, got ${JSON.stringify(specialists)}`,
  );
}
