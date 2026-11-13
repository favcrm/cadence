import type { Meta } from "./types";

export type BoardCapability =
  | "wiki"
  | "my_work"
  | "milestones"
  | "workflows"
  | "work_conversations"
  | "human_threads"
  | "credential_requests"
  | "app_automatic_updates";

/**
 * Whether this board serves an optional surface. Local/native boards serve all
 * surfaces by default (`meta?.hosted !== true`). Hosted tenant boards serve an
 * optional surface only when `/api/meta` explicitly advertises it as `true`.
 */
export function boardSupports(
  meta: Pick<Meta, "hosted" | BoardCapability> | null | undefined,
  capability: BoardCapability,
): boolean {
  if (meta?.hosted !== true) return true;
  return meta[capability] === true;
}
