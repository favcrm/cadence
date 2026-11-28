import { withPrivateOperation } from "../../lib/privateSession";

export type ActionBatchStatus = "pending" | "running" | "partial_failed" | "completed" | "denied";
export type ActionBatchItemStatus = "pending" | "running" | "applied" | "failed" | "denied";

export interface StagedBatchItem {
  seq: number;
  itemKey: string;
  childActionId: string;
  tool: string;
  label: string;
  recipient: string | null;
  status: ActionBatchItemStatus;
  attempts: number;
  errorCode: string | null;
  errorMessage: string | null;
  startedAt: number | null;
  finishedAt: number | null;
}

export interface StagedBatch {
  id: string;
  batchKey: string;
  title: string;
  summary: string | null;
  previewArtifactId: string | null;
  status: ActionBatchStatus;
  createdAt: number;
  decidedAt: number | null;
  updatedAt: number;
  totalCount: number;
  completedCount: number;
  failedCount: number;
  pendingCount: number;
  runningCount: number;
  lastError: string | null;
  decisionReason: string | null;
  sampleItems: StagedBatchItem[];
  failedItems: StagedBatchItem[];
}

export interface ActionBatchDetail {
  batch: StagedBatch;
  items: StagedBatchItem[];
}

export type FetchActionBatchOutcome =
  | { ok: true; detail: ActionBatchDetail }
  | { ok: false; kind: "access" | "missing" | "error" };

const BATCH_STATUSES: ReadonlySet<string> = new Set([
  "pending",
  "running",
  "partial_failed",
  "completed",
  "denied",
]);
const ITEM_STATUSES: ReadonlySet<string> = new Set([
  "pending",
  "running",
  "applied",
  "failed",
  "denied",
]);

const HIDDEN_CHARS = /[\u200B-\u200F\u202A-\u202E\u2060-\u206F\uFEFF]/g;
const clean = (text: string): string =>
  text.replace(HIDDEN_CHARS, (c) => `[U+${c.codePointAt(0)!.toString(16).toUpperCase().padStart(4, "0")}]`);

const isRecord = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);

const str = (v: unknown): string | null =>
  typeof v === "string" && v.trim() !== "" ? clean(v) : null;

const intOr = (v: unknown, fallback = 0): number =>
  typeof v === "number" && Number.isFinite(v) ? Math.max(0, Math.trunc(v)) : fallback;

const optTimestamp = (v: unknown): number | null =>
  typeof v === "number" && Number.isFinite(v) ? v : null;

export function parseBatchItems(value: unknown): StagedBatchItem[] {
  if (!Array.isArray(value)) return [];
  const items: StagedBatchItem[] = [];
  for (let i = 0; i < value.length; i += 1) {
    const raw = value[i];
    if (!isRecord(raw)) continue;
    const itemKey = str(raw.item_key);
    const label = str(raw.label) ?? itemKey;
    const statusRaw = typeof raw.status === "string" && ITEM_STATUSES.has(raw.status) ? raw.status : "pending";
    if (!itemKey || !label) continue;
    items.push({
      seq: intOr(raw.seq, i),
      itemKey,
      childActionId: str(raw.child_action_id) ?? itemKey,
      tool: str(raw.tool) ?? "action",
      label,
      recipient: str(raw.recipient),
      status: statusRaw as ActionBatchItemStatus,
      attempts: intOr(raw.attempts, 0),
      errorCode: str(raw.error_code),
      errorMessage: str(raw.error_message),
      startedAt: optTimestamp(raw.started_at),
      finishedAt: optTimestamp(raw.finished_at),
    });
  }
  return items;
}

export function parseBatchSummary(
  value: unknown,
  fallbackId?: string,
  fallbackCreatedAt?: number,
  fallbackStatus?: string,
  fallbackInput?: Record<string, unknown>,
  fallbackResultBatch?: Record<string, unknown>,
): StagedBatch | null {
  const src = isRecord(value) ? value : fallbackResultBatch;
  const input = fallbackInput ?? {};
  const id = (src && str(src.id)) ?? fallbackId ?? null;
  const title = (src && str(src.title)) ?? str(input.title);
  const batchKey = (src && str(src.batch_key)) ?? str(input.batch_key) ?? id;
  if (!id || !title || !batchKey) return null;

  const statusCandidate = src && typeof src.status === "string" ? src.status : undefined;
  const derivedFallbackStatus =
    fallbackStatus === "denied"
      ? "denied"
      : fallbackStatus === "approved"
        ? "completed"
        : "pending";
  const status = (
    statusCandidate && BATCH_STATUSES.has(statusCandidate) ? statusCandidate : derivedFallbackStatus
  ) as ActionBatchStatus;

  const totalCount = Math.max(
    1,
    intOr(src?.total_count, intOr(fallbackResultBatch?.total_count, intOr(input.total_count, 1))),
  );
  const completedCount = intOr(src?.completed_count, intOr(fallbackResultBatch?.completed_count, 0));
  const failedCount = intOr(src?.failed_count, intOr(fallbackResultBatch?.failed_count, 0));
  const runningCount = intOr(src?.running_count, intOr(fallbackResultBatch?.running_count, 0));
  const pendingCount = intOr(
    src?.pending_count,
    intOr(
      fallbackResultBatch?.pending_count,
      Math.max(0, totalCount - completedCount - failedCount - runningCount),
    ),
  );

  return {
    id,
    batchKey,
    title,
    summary: (src && str(src.summary)) ?? str(input.summary),
    previewArtifactId: (src && str(src.preview_artifact_id)) ?? str(input.preview_artifact_id),
    status,
    createdAt: optTimestamp(src?.created_at) ?? fallbackCreatedAt ?? 0,
    decidedAt: optTimestamp(src?.decided_at),
    updatedAt: optTimestamp(src?.updated_at) ?? fallbackCreatedAt ?? 0,
    totalCount,
    completedCount,
    failedCount,
    pendingCount,
    runningCount,
    lastError: (src && str(src.last_error)) ?? str(fallbackResultBatch?.last_error),
    decisionReason: src ? str(src.decision_reason) : null,
    sampleItems: parseBatchItems(src?.sample_items),
    failedItems: parseBatchItems(src?.failed_items),
  };
}

/** True while an ActionBatch still needs owner action or is actively processing in To do. */
export const isBatchActive = (batch: Pick<StagedBatch, "status"> | null | undefined): boolean =>
  batch?.status === "pending" || batch?.status === "running" || batch?.status === "partial_failed";

export async function fetchActionBatchDetail(
  id: string,
  conversation: string | null,
): Promise<FetchActionBatchOutcome> {
  const selector = conversation === null ? "" : `?conversation=${encodeURIComponent(conversation)}`;
  try {
    const response = await withPrivateOperation(async (operation) => {
      const res = await operation.fetch(
        `/api/assist/actions/${encodeURIComponent(id)}/batch${selector}`,
        {
          credentials: "same-origin",
          headers: { accept: "application/json" },
        },
      );
      const body: unknown =
        res.status === 401 || res.status === 403 || res.status === 404
          ? null
          : await res.json().catch(() => null);
      return { status: res.status, ok: res.ok, body };
    });
    if (response.status === 401 || response.status === 403) return { ok: false, kind: "access" };
    if (response.status === 404) return { ok: false, kind: "missing" };
    if (!response.ok || !isRecord(response.body) || !isRecord(response.body.batch)) {
      return { ok: false, kind: "error" };
    }
    const payload = response.body.batch;
    const summary = parseBatchSummary(payload.batch, id);
    if (!summary) return { ok: false, kind: "error" };
    const items = parseBatchItems(payload.items);
    return { ok: true, detail: { batch: summary, items } };
  } catch {
    return { ok: false, kind: "error" };
  }
}
