import type {
  HostedLocalCancelRequest,
  HostedLocalDispatchRequest,
  HostedLocalDispatchSpawnSpec,
  HostedLocalRetryRequest,
  IssueAssignmentRow,
} from "../../lib/types";

export const VALID_EFFORT_OPTIONS: readonly NonNullable<HostedLocalDispatchSpawnSpec["effort"]>[] = [
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
];

const ACTIVE_STATES = new Set(["waiting", "queued", "running", "blocked"]);
const TERMINAL_STATES = new Set(["result", "failed", "cancelled"]);

export function isActiveLocalAssignment(row: IssueAssignmentRow | null): boolean {
  return Boolean(row && ACTIVE_STATES.has(row.state));
}

export function isTerminalLocalAssignment(row: IssueAssignmentRow | null): boolean {
  return Boolean(row && TERMINAL_STATES.has(row.state));
}

export function resumeRunnerIdFor(row: IssueAssignmentRow | null): string | null {
  if (!row) return null;
  return row.spawn?.runner_id ?? row.resume?.runner_id ?? null;
}

export function canCancelLocalAssignment(
  row: IssueAssignmentRow | null,
  canLocalDispatch: boolean,
): boolean {
  return Boolean(
    canLocalDispatch &&
      row &&
      row.assignment_id &&
      typeof row.generation === "number" &&
      row.generation >= 1 &&
      isActiveLocalAssignment(row),
  );
}

export function canRetryLocalAssignment(
  row: IssueAssignmentRow | null,
  canLocalDispatch: boolean,
): boolean {
  return Boolean(
    canLocalDispatch &&
      row &&
      row.assignment_id &&
      typeof row.generation === "number" &&
      row.generation >= 1 &&
      row.state === "blocked",
  );
}

export function canResumeLocalAssignment(
  row: IssueAssignmentRow | null,
  canLocalDispatch: boolean,
): boolean {
  return Boolean(
    canLocalDispatch &&
      row &&
      row.assignment_id &&
      isTerminalLocalAssignment(row) &&
      resumeRunnerIdFor(row) !== null,
  );
}

export function makeOperationId(prefix = "op"): string {
  const raw =
    typeof crypto !== "undefined" && typeof crypto.randomUUID === "function"
      ? crypto.randomUUID()
      : `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
  return `${prefix}_${raw}`;
}

export function buildCancelPayload(
  row: IssueAssignmentRow,
  operationId: string,
): HostedLocalCancelRequest {
  if (!row.assignment_id || typeof row.generation !== "number") {
    throw new Error("Assignment id and generation are required to cancel.");
  }
  return {
    assignment_id: row.assignment_id,
    expected_generation: row.generation,
    operation_id: operationId,
  };
}

export interface RetryFormInput {
  note?: string;
  model?: string;
  effort?: HostedLocalDispatchSpawnSpec["effort"] | "";
}

export function buildRetryPayload(
  row: IssueAssignmentRow,
  operationId: string,
  input: RetryFormInput,
): HostedLocalRetryRequest {
  if (!row.assignment_id || typeof row.generation !== "number") {
    throw new Error("Assignment id and generation are required to retry.");
  }
  const note = input.note?.trim();
  const model = row.spawn ? input.model?.trim() : undefined;
  const effort = row.spawn && input.effort ? input.effort : undefined;
  return {
    assignment_id: row.assignment_id,
    expected_generation: row.generation,
    operation_id: operationId,
    ...(note ? { note } : {}),
    ...(model ? { model } : {}),
    ...(effort ? { effort } : {}),
  };
}

export interface ResumeFormInput {
  note?: string;
  model?: string;
  effort?: HostedLocalDispatchSpawnSpec["effort"] | "";
}

export function buildResumePayload(
  row: IssueAssignmentRow,
  idempotencyKey: string,
  input: ResumeFormInput,
): Extract<HostedLocalDispatchRequest, { mode: "resume" }> {
  const runnerId = resumeRunnerIdFor(row);
  if (!row.assignment_id || !runnerId) {
    throw new Error("Assignment id and runner id are required to resume a spawned lane.");
  }
  const note = input.note?.trim();
  const model = input.model?.trim();
  const effort = input.effort || undefined;
  return {
    idempotency_key: idempotencyKey,
    mode: "resume",
    resume: {
      runner_id: runnerId,
      prior_assignment_id: row.assignment_id,
      ...(row.resume?.lane_alias ? { lane_alias: row.resume.lane_alias } : {}),
      ...(row.target_agent_id ? { target_agent_id: row.target_agent_id } : {}),
      ...(model ? { model } : {}),
      ...(effort ? { effort } : {}),
    },
    ...(note ? { note } : {}),
  };
}
