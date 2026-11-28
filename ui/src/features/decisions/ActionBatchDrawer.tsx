import { useEffect, useState, type ReactNode } from "react";
import DrawerShell from "../app-shell/shared/DrawerShell";
import { READ_ONLY_COPY } from "../home/AnswerForm";
import { useTranslate } from "../../lib/localeText";
import { canvasSnapshot, feedCanvas, openArtifact } from "../canvas/canvasStore";
import {
  fetchActionBatchDetail,
  type ActionBatchItemStatus,
  type ActionBatchStatus,
  type StagedBatchItem,
} from "./actionBatch";
import { ageText, askedBy, isolate, visible, type StagedWrite } from "./stagedWrite";
import type { Attempt, DecisionNotice } from "./useDecisions";

type Say = (english: string) => string;
type ItemFilter = "all" | "failed" | "pending" | "applied";

const B = ({ children }: { children: ReactNode }) => (
  <bdi>{typeof children === "string" ? isolate(visible(children)) : children}</bdi>
);

function itemStatusTone(status: ActionBatchItemStatus): "ok" | "warn" | "fail" | "info" | "muted" {
  switch (status) {
    case "applied":
      return "ok";
    case "failed":
      return "fail";
    case "running":
      return "info";
    case "denied":
      return "muted";
    default:
      return "warn";
  }
}

function itemStatusLabel(status: ActionBatchItemStatus, t: Say): string {
  switch (status) {
    case "applied":
      return t("Applied");
    case "failed":
      return t("Failed");
    case "running":
      return t("Running");
    case "denied":
      return t("Denied");
    default:
      return t("Pending");
  }
}

function batchStatusMeta(
  status: ActionBatchStatus,
  completedCount: number,
  failedCount: number,
  totalCount: number,
  t: Say,
): { label: string; tone: "ok" | "warn" | "fail" | "info" | "muted" } {
  switch (status) {
    case "running":
      return {
        label: t("Running · {done}/{total}")
          .replace("{done}", String(completedCount))
          .replace("{total}", String(totalCount)),
        tone: "info",
      };
    case "partial_failed":
      return {
        label: t("Partial failure · {failed} failed").replace("{failed}", String(failedCount)),
        tone: "fail",
      };
    case "completed":
      return {
        label: t("Completed · {done}/{total}")
          .replace("{done}", String(completedCount))
          .replace("{total}", String(totalCount)),
        tone: "ok",
      };
    case "denied":
      return { label: t("Denied"), tone: "muted" };
    default:
      return { label: t("Pending approval"), tone: "warn" };
  }
}

function BatchItemRow({ item, t }: { item: StagedBatchItem; t: Say }) {
  const tone = itemStatusTone(item.status);
  return (
    <li className="dx-batch-item" data-batch-item={item.itemKey} data-batch-item-status={item.status}>
      <div className="dx-batch-item-main">
        <div className="dx-batch-item-top">
          <span className="dx-batch-item-title">
            <B>{item.label}</B>
          </span>
          <span className="dx-batch-item-pill" data-tone={tone}>
            {itemStatusLabel(item.status, t)}
          </span>
        </div>
        <div className="dx-batch-item-meta">
          {item.recipient ? (
            <span>
              <B>{item.recipient}</B>
            </span>
          ) : null}
          {item.recipient ? <span aria-hidden="true">·</span> : null}
          <span className="dx-id">
            <B>{item.tool}</B>
          </span>
          {item.attempts > 0 ? (
            <>
              <span aria-hidden="true">·</span>
              <span className="num">
                {item.attempts === 1
                  ? t("1 attempt")
                  : t("{n} attempts").replace("{n}", String(item.attempts))}
              </span>
            </>
          ) : null}
        </div>
        {(item.errorMessage || item.errorCode) && (
          <p className="dx-batch-item-err m-0" role="status">
            <B>{item.errorMessage ?? item.errorCode ?? ""}</B>
          </p>
        )}
      </div>
    </li>
  );
}

/**
 * AOS-779 (CAD-2894): Owner's slide-over drawer for a grouped ActionBatch write:
 * 5-row sample preview + Canvas artifact link, 1-click Approve Batch / Deny Batch,
 * live progress bar, failed-item inspection, all-item drill-down, and Retry Failed Only.
 */
export default function ActionBatchDrawer({
  write,
  conversation,
  attempt,
  notice,
  busy,
  lost,
  uncertain,
  elsewhere,
  block,
  at,
  total,
  onApprove,
  onDeny,
  onRetryFailed,
  onRefresh,
  onCheck,
  onContinue,
  onSkip,
  onClose,
}: {
  write: StagedWrite;
  conversation: string | null;
  attempt: Attempt | undefined;
  notice: DecisionNotice | null;
  busy: boolean;
  lost: boolean;
  uncertain: boolean;
  elsewhere: boolean;
  block: string | null;
  at: number;
  total: number;
  onApprove: (clicks: number) => void;
  onDeny: (reason: string | undefined, clicks: number) => void;
  onRetryFailed: (clicks: number) => void;
  onRefresh: () => void;
  onCheck: () => void;
  onContinue: () => void;
  onSkip: () => void;
  onClose: () => void;
}) {
  const t = useTranslate();
  const batch = write.batch!;
  const [denyOpen, setDenyOpen] = useState(false);
  const [denyReason, setDenyReason] = useState("");
  const [allOpen, setAllOpen] = useState(false);
  const [itemFilter, setItemFilter] = useState<ItemFilter>("all");
  const [detailState, setDetailState] = useState<{
    loading: boolean;
    items: StagedBatchItem[] | null;
    errorKind: "access" | "load" | null;
  }>({ loading: false, items: null, errorKind: null });

  useEffect(() => {
    setDenyOpen(false);
    setDenyReason("");
    setAllOpen(false);
    setItemFilter("all");
    setDetailState({ loading: false, items: null, errorKind: null });
  }, [write.id]);

  // Refresh full-item list when owner opens "Inspect all" or counts advance while expanded.
  useEffect(() => {
    if (!allOpen) return;
    let cancelled = false;
    setDetailState((prev) => ({ ...prev, loading: true, errorKind: null }));
    void fetchActionBatchDetail(write.id, conversation).then((res) => {
      if (cancelled) return;
      if (res.ok) {
        setDetailState({ loading: false, items: res.detail.items, errorKind: null });
      } else {
        setDetailState((prev) => ({
          loading: false,
          items: prev.items,
          errorKind: res.kind === "access" ? "access" : "load",
        }));
      }
    });
    return () => {
      cancelled = true;
    };
  }, [allOpen, write.id, conversation, batch.updatedAt, batch.completedCount, batch.failedCount]);

  const statusMeta = batchStatusMeta(
    batch.status,
    batch.completedCount,
    batch.failedCount,
    batch.totalCount,
    t,
  );
  const canApproveOrDeny =
    batch.status === "pending" && block === null && !busy && !lost && !uncertain && !elsewhere;
  const canRetryFailed =
    batch.status === "partial_failed" && block === null && !busy && !lost && !uncertain && !elsewhere;
  const approving = attempt?.phase === "busy" && attempt.verb === "approve";
  const denying = attempt?.phase === "busy" && attempt.verb === "deny";
  const pct =
    batch.totalCount > 0 ? Math.min(100, Math.round((batch.completedCount / batch.totalCount) * 100)) : 0;

  const openCanvasPreview = () => {
    if (!batch.previewArtifactId) return;
    const snap = canvasSnapshot();
    const existing = snap.tabs.find((tab) => tab.id === batch.previewArtifactId);
    if (snap.conversation === null) {
      feedCanvas({
        conversation: conversation ?? "batch:preview",
        surface: "home",
        composerKey: null,
        cards: [],
        working: null,
        turn: null,
      });
    }
    openArtifact({
      id: batch.previewArtifactId,
      version: existing?.latest ?? 1,
      kind: existing?.kind && existing.kind !== "app" && existing.kind !== "record" && existing.kind !== "workbook" ? existing.kind : "html",
      title: existing?.title || visible(batch.title),
    });
    onClose();
  };

  const stateBanner = lost ? (
    <p className="issue-peek-note err m-0" role="alert">
      {t("Only the workspace owner can decide this request.")}
    </p>
  ) : elsewhere || attempt?.phase === "elsewhere" ? (
    <p className="issue-peek-note warn m-0" role="alert">
      {t("This was already decided elsewhere. The list has been refreshed.")}
    </p>
  ) : uncertain ? (
    <p className="issue-peek-note warn m-0" role="alert">
      {t("We could not confirm the result. It may already have gone through, so do not approve again; check first.")}
    </p>
  ) : block !== null ? (
    <p className="issue-peek-note warn m-0" role="status">
      {t(READ_ONLY_COPY)}
    </p>
  ) : null;

  const via =
    write.origin === null
      ? t(conversation === null ? "in Home" : "in this conversation")
      : t("via {via}").replace("{via}", write.origin.via.toLowerCase() === "cli" ? "CLI" : write.origin.via);

  const visibleDetailedItems = (detailState.items ?? []).filter((item) =>
    itemFilter === "all" ? true : item.status === itemFilter,
  );

  const foot = (
    <>
      <div className="crm-drawer-actions">
        {elsewhere || attempt?.phase === "elsewhere" ? (
          <button type="button" className="btn btn-sm btn-primary" onClick={onContinue}>
            {t("Continue")}
          </button>
        ) : uncertain ? (
          <button
            type="button"
            className="btn btn-sm btn-primary"
            aria-disabled={busy || undefined}
            onClick={onCheck}
          >
            {t("Check again")}
          </button>
        ) : batch.status === "pending" ? (
          <>
            <button
              type="button"
              className="btn btn-sm btn-primary"
              data-batch-approve
              aria-disabled={!canApproveOrDeny || undefined}
              aria-busy={approving || undefined}
              title={block !== null ? t(READ_ONLY_COPY) : undefined}
              onClick={(e) => onApprove(e.detail)}
            >
              {approving ? (
                <>
                  <span className="btn-spin" aria-hidden="true" />
                  {t("Starting batch…")}
                </>
              ) : (
                t("Approve Batch ({n})").replace("{n}", String(batch.totalCount))
              )}
            </button>
            <button
              type="button"
              className="btn btn-sm"
              data-batch-deny
              aria-disabled={!canApproveOrDeny || undefined}
              aria-busy={denying || undefined}
              title={block !== null ? t(READ_ONLY_COPY) : undefined}
              onClick={(e) => onDeny(denyOpen ? denyReason : undefined, e.detail)}
            >
              {denying ? (
                <>
                  <span className="btn-spin" aria-hidden="true" />
                  {t("Denying…")}
                </>
              ) : (
                t("Deny Batch")
              )}
            </button>
          </>
        ) : batch.status === "partial_failed" ? (
          <button
            type="button"
            className="btn btn-sm btn-primary"
            data-batch-retry
            aria-disabled={!canRetryFailed || undefined}
            aria-busy={approving || undefined}
            title={block !== null ? t(READ_ONLY_COPY) : undefined}
            onClick={(e) => onRetryFailed(e.detail)}
          >
            {approving ? (
              <>
                <span className="btn-spin" aria-hidden="true" />
                {t("Retrying…")}
              </>
            ) : (
              t("Retry Failed Only ({n})").replace("{n}", String(Math.max(1, batch.failedCount)))
            )}
          </button>
        ) : batch.status === "running" ? (
          <button
            type="button"
            className="btn btn-sm"
            data-batch-refresh
            onClick={onRefresh}
          >
            {t("Refresh")}
          </button>
        ) : (
          <button type="button" className="btn btn-sm btn-primary" onClick={onContinue}>
            {t("Continue")}
          </button>
        )}
        <button
          type="button"
          className="btn btn-sm btn-ghost"
          aria-disabled={busy || undefined}
          onClick={batch.status === "pending" ? onSkip : onClose}
        >
          {batch.status === "pending" ? t("Not now") : t("Close")}
        </button>
      </div>
      <span className="crm-drawer-note" aria-live="polite">
        {t("{n} of {m}").replace("{n}", String(at + 1)).replace("{m}", String(total))}
      </span>
    </>
  );

  return (
    <DrawerShell
      kind="staged"
      modal
      trapFocus
      scrim="board"
      size="md"
      preventClose={busy}
      label={t("Batch decision")}
      title={<B>{batch.title}</B>}
      subtitle={
        <div className="dx-asked">
          {t("Asked by")} <B>{askedBy(write, t) ?? t("the assistant")}</B>, <B>{via}</B> ·{" "}
          {ageText(write.createdAt, t)}
        </div>
      }
      pills={
        <>
          <span className="s-status-pill" data-tone={statusMeta.tone === "muted" ? undefined : statusMeta.tone} data-batch-status={batch.status}>
            {statusMeta.label}
          </span>
          <span className="dx-domain-chip num">
            {batch.totalCount === 1
              ? t("1 item")
              : t("{n} items").replace("{n}", String(batch.totalCount))}
          </span>
        </>
      }
      foot={foot}
      onClose={onClose}
    >
      <div className="wi-panel dx-panel" data-batch-drawer>
        {notice && batch.status !== "running" && (
          <p className={`issue-peek-note ${notice.outcome === "applied" || notice.outcome === "denied" ? "ok" : "warn"} m-0`} role="status">
            <B>{notice.title}</B>
          </p>
        )}
        {stateBanner}

        {batch.summary && (
          <p className="rv-note m-0" data-batch-summary>
            <B>{batch.summary}</B>
          </p>
        )}

        {batch.previewArtifactId && (
          <div className="dx-batch-artifact">
            <div>
              <div className="text-label text-ink-200">{t("Canvas preview artifact")}</div>
              <div className="dx-id">
                <B>{batch.previewArtifactId}</B>
              </div>
            </div>
            <button
              type="button"
              className="btn btn-sm"
              data-batch-preview-canvas
              onClick={openCanvasPreview}
            >
              {t("Open full preview in Canvas")} ↗
            </button>
          </div>
        )}

        <section className="dx-sec" aria-label={t("Batch progress")}>
          <div className="dx-batch-prog-head">
            <h4 className="rv-h m-0">{t("Progress")}</h4>
            <span className="text-label text-ink-300 num" data-batch-progress-text>
              {t("{done}/{total} completed")
                .replace("{done}", String(batch.completedCount))
                .replace("{total}", String(batch.totalCount))}
            </span>
          </div>
          <div
            className="dx-batch-prog-track"
            role="progressbar"
            aria-label={t("Batch execution progress")}
            aria-valuemin={0}
            aria-valuemax={batch.totalCount}
            aria-valuenow={batch.completedCount}
            aria-valuetext={`${batch.completedCount} / ${batch.totalCount}`}
          >
            <div
              className={`dx-batch-prog-fill${batch.failedCount > 0 ? " warn" : batch.status === "completed" ? " ok" : ""}`}
              style={{ width: `${pct}%` }}
            />
          </div>
          <div className="dx-batch-metrics">
            <div className="dx-batch-metric">
              <span className="dx-batch-metric-k">{t("Completed")}</span>
              <strong className="dx-batch-metric-v num">{batch.completedCount}</strong>
            </div>
            <div className="dx-batch-metric">
              <span className="dx-batch-metric-k">{t("Pending")}</span>
              <strong className="dx-batch-metric-v num">{batch.pendingCount + batch.runningCount}</strong>
            </div>
            <div className="dx-batch-metric" data-fail={batch.failedCount > 0 ? "" : undefined}>
              <span className="dx-batch-metric-k">{t("Failed")}</span>
              <strong className="dx-batch-metric-v num">{batch.failedCount}</strong>
            </div>
            <div className="dx-batch-metric">
              <span className="dx-batch-metric-k">{t("Total")}</span>
              <strong className="dx-batch-metric-v num">{batch.totalCount}</strong>
            </div>
          </div>
        </section>

        {(batch.failedCount > 0 || batch.failedItems.length > 0 || batch.lastError) && (
          <section className="dx-sec" aria-label={t("Failed items")} data-batch-failures>
            <h4 className="rv-h">{t("Failed items")}</h4>
            {batch.lastError && (
              <p className="issue-peek-note warn m-0" role="alert">
                <B>{batch.lastError}</B>
              </p>
            )}
            <p className="rv-note m-0">
              {t("Completed items stay applied and will be skipped when you retry failed items.")}
            </p>
            {batch.failedItems.length > 0 && (
              <ul className="dx-batch-list">
                {batch.failedItems.map((item) => (
                  <BatchItemRow key={`fail:${item.itemKey}`} item={item} t={t} />
                ))}
              </ul>
            )}
          </section>
        )}

        <section className="dx-sec" aria-label={t("Sample preview")} data-batch-samples>
          <div className="dx-batch-prog-head">
            <h4 className="rv-h m-0">
              {t("Sample preview (first {n})").replace("{n}", String(Math.min(5, Math.max(batch.sampleItems.length, 1))))}
            </h4>
            {batch.totalCount > 5 && (
              <span className="text-micro text-ink-400 num">
                +{batch.totalCount - Math.min(5, batch.sampleItems.length)} {t("more")}
              </span>
            )}
          </div>
          {batch.sampleItems.length > 0 ? (
            <ul className="dx-batch-list">
              {batch.sampleItems.slice(0, 5).map((item) => (
                <BatchItemRow key={`sample:${item.itemKey}`} item={item} t={t} />
              ))}
            </ul>
          ) : (
            <p className="rv-note m-0">{t("No inline sample items; expand all items to inspect.")}</p>
          )}

          <div>
            <button
              type="button"
              className="rv-dlnk dx-rawbtn"
              data-batch-inspect-all
              aria-expanded={allOpen}
              onClick={() => setAllOpen((open) => !open)}
            >
              {allOpen
                ? t("Hide all items")
                : t("Inspect all {n} items").replace("{n}", String(batch.totalCount))}
            </button>
          </div>

          {allOpen && (
            <div className="dx-batch-all" data-batch-all-panel>
              <div className="seg-pills" role="group" aria-label={t("Filter batch items")}>
                {(
                  [
                    ["all", t("All")],
                    ["failed", t("Failed")],
                    ["pending", t("Pending")],
                    ["applied", t("Completed")],
                  ] as const
                ).map(([key, label]) => (
                  <button
                    key={key}
                    type="button"
                    className="seg-pill"
                    aria-pressed={itemFilter === key}
                    onClick={() => setItemFilter(key)}
                  >
                    {label}
                  </button>
                ))}
              </div>
              {detailState.loading && !detailState.items && (
                <p className="rv-note m-0">{t("Loading batch items…")}</p>
              )}
              {detailState.errorKind && (
                <p className="issue-peek-note err m-0" role="alert">
                  {detailState.errorKind === "access"
                    ? t("Only the workspace owner can inspect batch items.")
                    : t("Could not load full batch items.")}
                </p>
              )}
              {detailState.items && (
                visibleDetailedItems.length > 0 ? (
                  <ul className="dx-batch-list dx-batch-list-scroll">
                    {visibleDetailedItems.map((item) => (
                      <BatchItemRow key={`all:${item.itemKey}`} item={item} t={t} />
                    ))}
                  </ul>
                ) : (
                  <p className="rv-note m-0">{t("No batch items match this filter.")}</p>
                )
              )}
            </div>
          )}
        </section>

        {batch.status === "pending" && block === null && (
          <div>
            <button
              type="button"
              className="rv-dlnk dx-rawbtn dx-deny-toggle"
              aria-expanded={denyOpen}
              onClick={() => {
                setDenyOpen((prev) => {
                  if (prev) setDenyReason("");
                  return !prev;
                });
              }}
            >
              {denyOpen ? t("Hide deny note") : t("Add deny note (optional)")}
            </button>
            {denyOpen && (
              <div className="dx-deny-box">
                <textarea
                  value={denyReason}
                  onChange={(e) => setDenyReason(e.target.value)}
                  maxLength={500}
                  rows={2}
                  placeholder={t("Why are you denying this batch? (optional)")}
                  aria-label={t("Deny note (optional)")}
                />
              </div>
            )}
          </div>
        )}
      </div>
    </DrawerShell>
  );
}
