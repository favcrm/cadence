import { useContext, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import { requestThreadReload } from "../../lib/masterChanged";
import { navigate } from "../../lib/useLocation";
import { WriteGate } from "../auth/WriteGate";
import type { ThreadEntry } from "./thread";

interface ContextReceiptPayload {
  op: string;
  historyId: string | null;
  target: string;
  addedCount: number;
  removedCount: number;
  addedPreview: string[];
  removedPreview: string[];
  undoneHistoryId: string | null;
}

function asStringList(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value.filter((item): item is string => typeof item === "string" && item.trim() !== "");
}

function parseReceipt(entry: ThreadEntry): ContextReceiptPayload | null {
  const payload = entry.payload;
  if (!payload || typeof payload !== "object") return null;
  const op = typeof payload.op === "string" ? payload.op : "";
  const target = typeof payload.target === "string" ? payload.target : "";
  if (!op || !target) return null;
  const historyId =
    typeof payload.history_id === "string" && payload.history_id.trim() !== ""
      ? payload.history_id
      : null;
  const undoneHistoryId =
    typeof payload.undone_history_id === "string" && payload.undone_history_id.trim() !== ""
      ? payload.undone_history_id
      : null;
  const addedPreview = asStringList(payload.added_preview);
  const removedPreview = asStringList(payload.removed_preview);
  const addedCount =
    typeof payload.added_count === "number" && Number.isFinite(payload.added_count)
      ? payload.added_count
      : addedPreview.length;
  const removedCount =
    typeof payload.removed_count === "number" && Number.isFinite(payload.removed_count)
      ? payload.removed_count
      : removedPreview.length;
  return {
    op,
    historyId,
    target,
    addedCount,
    removedCount,
    addedPreview,
    removedPreview,
    undoneHistoryId,
  };
}

function receiptSummary(receipt: ContextReceiptPayload, t: (s: string) => string): string {
  if (receipt.op === "undo") return t("Change undone");
  if (receipt.target === "SOUL.md" || receipt.op === "update_soul") return t("SOUL updated");
  if (receipt.target.startsWith("memory/")) {
    const slug = receipt.target.replace(/^memory\//, "").replace(/\.md$/, "");
    return t("Topic updated ({name})").replace("{name}", slug);
  }
  const parts: string[] = [];
  if (receipt.addedCount > 0) {
    parts.push(
      receipt.addedCount === 1
        ? t("+1 fact")
        : t("+{n} facts").replace("{n}", String(receipt.addedCount)),
    );
  }
  if (receipt.removedCount > 0) {
    parts.push(
      receipt.removedCount === 1
        ? t("-1 fact")
        : t("-{n} facts").replace("{n}", String(receipt.removedCount)),
    );
  }
  if (parts.length > 0) return parts.join(" / ");
  return t("Memory updated");
}

/**
 * CAD-2834: Compact inline context receipt under a personal assistant turn
 * showing SOUL / MEMORY / topic updates with View and 1-click Undo.
 */
export function ContextReceiptCard({ entry }: { entry: ThreadEntry }) {
  const t = useTranslate();
  const writeBlock = useContext(WriteGate);
  const receipt = parseReceipt(entry);
  const [expanded, setExpanded] = useState(false);
  const [busy, setBusy] = useState(false);
  const [undone, setUndone] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (!receipt) {
    return (
      <p className="ctx-receipt ctx-receipt-plain text-micro text-ink-500 break-words" data-kind="context_receipt">
        {entry.text || t("Memory updated")}
      </p>
    );
  }

  const hasDiff = receipt.addedPreview.length > 0 || receipt.removedPreview.length > 0;
  const canUndo =
    receipt.op !== "undo" && receipt.historyId !== null && !undone && writeBlock === null;
  const label = receiptSummary(receipt, t);

  const onUndo = async () => {
    if (!receipt.historyId || busy || undone || writeBlock !== null) return;
    setBusy(true);
    setError(null);
    try {
      await api.memberAssistMemoryUndo(receipt.historyId);
      setUndone(true);
      requestThreadReload();
    } catch (err) {
      setError(err instanceof ApiError ? err.message : t("Couldn't undo that change."));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      className="ctx-receipt"
      data-kind="context_receipt"
      data-op={receipt.op}
      data-undone={undone ? "true" : undefined}
    >
      <div className="ctx-receipt-row">
        <span className="ctx-receipt-badge num">{label}</span>
        <span className="ctx-receipt-target num">{receipt.target}</span>
        {hasDiff && (
          <button
            type="button"
            className="lnk ctx-receipt-act"
            aria-expanded={expanded}
            onClick={() => setExpanded((cur) => !cur)}
          >
            {expanded ? t("Hide") : t("Diff")}
          </button>
        )}
        <a
          className="lnk ctx-receipt-act"
          href="/agents/assistant?tab=memory"
          onClick={(event) => {
            if (!event.metaKey && !event.ctrlKey && !event.shiftKey && !event.altKey) {
              event.preventDefault();
              navigate("/agents/assistant?tab=memory");
            }
          }}
        >
          {t("View")}
        </a>
        {undone ? (
          <span className="ctx-receipt-status num">{t("Undone")}</span>
        ) : (
          canUndo && (
            <button
              type="button"
              className="lnk ctx-receipt-act ctx-receipt-undo"
              disabled={busy}
              aria-busy={busy || undefined}
              onClick={() => void onUndo()}
            >
              {busy ? t("Undoing…") : t("Undo")}
            </button>
          )
        )}
      </div>
      {expanded && hasDiff && (
        <ul className="ctx-receipt-diff num">
          {receipt.addedPreview.map((line, i) => (
            <li key={`a${i}`} className="ctx-diff-add">
              + {line}
            </li>
          ))}
          {receipt.removedPreview.map((line, i) => (
            <li key={`r${i}`} className="ctx-diff-rem">
              − {line}
            </li>
          ))}
        </ul>
      )}
      {error && (
        <p className="ctx-receipt-err text-micro text-fail" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
