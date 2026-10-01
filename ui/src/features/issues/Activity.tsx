import { useRef, useState } from "react";
import { type ResourceState } from "../../lib/cache";
import { api, ApiError } from "../../lib/api";
import { fmtTime } from "../../lib/fmt";
import { resources } from "../../lib/resources";
import type { IssueHistoryEntry } from "../../lib/types";
import Button from "../../ui/Button";
import Md from "../../ui/Md";
import { timelineRows } from "./model";
import { type IssuePageProps as Props } from "./issuePageProps";
import { ReadNotice } from "./issuePageShared";

const TONE: Record<string, string> = {
  done: "bg-ok",
  now: "bg-accent",
  warn: "bg-warn",
  wait: "bg-ink-600",
};

export function Activity({
  id,
  rows,
  history,
  retryHistory,
  readOnly,
  rev,
  onWrite,
  onError,
  onOpen,
}: {
  id: string;
  rows: ReturnType<typeof timelineRows>;
  history: ResourceState<IssueHistoryEntry[]>;
  retryHistory: () => void;
  readOnly: boolean;
  rev: string;
  onWrite: Props["onWrite"];
  onError: Props["onError"];
  onOpen: (id: string) => void;
}) {
  const [comment, setComment] = useState("");
  const [preview, setPreview] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const pending = useRef(false);
  const send = async () => {
    const body = comment.trim();
    if (!body || readOnly || pending.current) return;
    pending.current = true;
    setBusy(true);
    setError(null);
    try {
      const result = await api.comment(id, body, rev);
      onWrite(result, `${id} comment`);
      setComment("");
      setPreview(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Comment could not be posted.");
      onError(e, "comment");
      // Keep the draft and optimistic concurrency; the next explicit retry uses
      // a fresh revision after another writer changed the issue. Never replay it.
      if (e instanceof ApiError && e.status === 409) {
        const resource = resources.issue(id);
        await resource.invalidate();
        // Invalidation of an older in-flight read schedules one fresh follow-up.
        if (resource.get().inFlight) await resource.refresh();
      }
    } finally {
      pending.current = false;
      setBusy(false);
    }
  };
  return (
    <section className="grid gap-4">
      <ReadNotice name="history" state={history} retry={retryHistory} />
      {rows.length === 0 && history.data !== null && !history.error ? (
        <div className="card px-4 py-8 text-center">
          <div className="text-cardtitle font-semibold text-ink-100">
            No activity yet
          </div>
          <p className="text-secondary text-ink-400 mt-1">
            Comments and changes to this issue will appear here.
          </p>
        </div>
      ) : rows.length > 0 ? (
        <>
          <h2 className="text-cardtitle font-semibold text-ink-100 m-0">
            Recent activity
          </h2>
          <ol className="grid issue-timeline">
            {rows.map((row, i) => (
              <li key={`${row.title}-${i}`} className="relative pl-5 pb-4">
                <i
                  className={`absolute left-0 top-1.5 w-2 h-2 rounded-full ${TONE[row.tone]}`}
                />
                {i < rows.length - 1 && (
                  <i className="absolute left-[3px] top-4 bottom-0 w-px bg-ink-700" />
                )}
                <div className="flex items-baseline justify-between gap-3">
                  <b className="text-ink-100 font-semibold">{row.title}</b>
                  {row.at && (
                    <span className="num text-micro text-ink-500">
                      {fmtTime(row.at)}
                    </span>
                  )}
                </div>
                <div className="issue-reader text-secondary text-ink-400 mt-0.5">
                  {row.markdown ? (
                    <Md text={row.detail} onOpen={onOpen} />
                  ) : (
                    row.detail
                  )}
                </div>
              </li>
            ))}
          </ol>
        </>
      ) : null}
      {!readOnly && (
        <div className="card p-4 grid gap-2.5">
          <div className="flex items-baseline gap-2">
            <label className="slabel" htmlFor="issue-comment">
              Comment
            </label>
            <Button
              variant="ghost"
              size="sm"
              disabled={busy}
              onClick={() => setPreview((v) => !v)}
            >
              {preview ? "Edit comment" : "Preview"}
            </Button>
          </div>
          {preview ? (
            <div className="issue-reader card p-3 text-secondary text-ink-300 min-h-16">
              {comment.trim() ? (
                <Md text={comment} onOpen={onOpen} />
              ) : (
                <span className="text-ink-500">
                  Write a comment to preview it.
                </span>
              )}
            </div>
          ) : (
            <textarea
              id="issue-comment"
              disabled={busy}
              className="field w-full !h-auto py-2 text-secondary"
              rows={3}
              value={comment}
              onChange={(e) => setComment(e.target.value)}
              placeholder="Add an update or a question…"
            />
          )}
          {error && (
            <p className="text-secondary text-fail m-0" role="alert">
              {error}
            </p>
          )}
          <div className="flex">
            <Button
              variant="primary"
              loading={busy}
              disabled={!comment.trim()}
              onClick={send}
            >
              {busy ? "Posting…" : "Post comment"}
            </Button>
          </div>
        </div>
      )}
    </section>
  );
}
