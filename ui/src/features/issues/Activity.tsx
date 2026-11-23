import { useEffect, useRef, useState } from "react";
import { type ResourceState } from "../../lib/cache";
import { api, ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import type { IssueHistoryEntry } from "../../lib/types";
import Button from "../../ui/Button";
import Md from "../../ui/Md";
import MentionPicker from "../mentions/MentionPicker";
import {
  applyMention,
  filterMentionCandidates,
  findActiveMention,
  startsWithNote,
  type MentionCandidate,
} from "../mentions/mentions";
import { timelineRows } from "./model";
import { type IssuePageProps as Props } from "./issuePageProps";
import { ReadNotice } from "./issuePageShared";
import { useLocale } from "../../lib/locale";

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
  mentionCandidates,
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
  mentionCandidates?: MentionCandidate[];
}) {
  const { t, formatDate } = useLocale();
  const [comment, setComment] = useState("");
  const [silentNote, setSilentNote] = useState(false);
  const [preview, setPreview] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [caret, setCaret] = useState(0);
  const [mentionHi, setMentionHi] = useState(0);
  const [mentionOffFor, setMentionOffFor] = useState<string | null>(null);
  const commentRef = useRef<HTMLTextAreaElement | null>(null);
  const pending = useRef(false);
  const activeMention =
    !preview && !busy && mentionCandidates && mentionCandidates.length > 0
      ? findActiveMention(comment, caret)
      : null;
  const mentionSpanKey = activeMention ? `${activeMention.start}:${activeMention.query}` : null;
  useEffect(() => {
    setMentionHi(0);
  }, [mentionSpanKey]);
  const mentionMatches =
    activeMention && mentionCandidates && mentionSpanKey !== mentionOffFor
      ? filterMentionCandidates(mentionCandidates, activeMention.query)
      : [];
  const selectableIndices = mentionMatches
    .map((candidate, index) => (candidate.active ? index : -1))
    .filter((index) => index >= 0);
  const mentionHiClamped =
    selectableIndices.length === 0
      ? -1
      : selectableIndices.includes(mentionHi)
        ? mentionHi
        : selectableIndices[0]!;
  const completeMention = (candidate: MentionCandidate) => {
    if (!activeMention || !candidate.active) return;
    const applied = applyMention(comment, activeMention, candidate);
    setComment(applied.text);
    setCaret(applied.caret);
    setMentionOffFor(null);
    requestAnimationFrame(() => {
      const el = commentRef.current;
      if (!el) return;
      el.focus();
      el.setSelectionRange(applied.caret, applied.caret);
    });
  };
  const send = async () => {
    const rawBody = comment.trim();
    if (!rawBody || readOnly || pending.current) return;
    const body = silentNote && !startsWithNote(rawBody) ? `/note ${rawBody}` : rawBody;
    pending.current = true;
    setBusy(true);
    setError(null);
    try {
      const result = await api.comment(id, body, rev);
      onWrite(result, `${id} comment`);
      setComment("");
      setSilentNote(false);
      setPreview(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : t("Comment could not be posted."));
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
            {t("No activity yet")}
          </div>
          <p className="text-secondary text-ink-400 mt-1">
            {t("Comments and changes to this issue will appear here.")}
          </p>
        </div>
      ) : rows.length > 0 ? (
        <>
          <h2 className="text-cardtitle font-semibold text-ink-100 m-0">
            {t("Recent activity")}
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
                      {formatDate(row.at, { dateStyle: "medium", timeStyle: "short" })}
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
              {t("Comment")}
            </label>
            <Button
              variant="ghost"
              size="sm"
              disabled={busy}
              onClick={() => setPreview((v) => !v)}
            >
              {t(preview ? "Edit comment" : "Preview")}
            </Button>
            <button
              type="button"
              className="composer-silent-toggle ml-auto"
              aria-pressed={silentNote}
              disabled={busy}
              title={t("Silent note — post without notifying members or waking agents")}
              onClick={() => setSilentNote((v) => !v)}
            >
              <span>/note</span>
              <span>{t("Silent note")}</span>
            </button>
          </div>
          {preview ? (
            <div className="issue-reader card p-3 text-secondary text-ink-300 min-h-16">
              {comment.trim() ? (
                <Md text={comment} onOpen={onOpen} />
              ) : (
                <span className="text-ink-500">
                  {t("Write a comment to preview it.")}
                </span>
              )}
            </div>
          ) : (
            <div className="relative">
              {mentionMatches.length > 0 && (
                <MentionPicker
                  candidates={mentionMatches}
                  highlighted={mentionHiClamped}
                  onHighlight={setMentionHi}
                  onSelect={completeMention}
                />
              )}
              <textarea
                ref={commentRef}
                id="issue-comment"
                disabled={busy}
                className="field w-full !h-auto py-2 text-secondary"
                rows={3}
                value={comment}
                onChange={(e) => {
                  setMentionOffFor(null);
                  setCaret(e.target.selectionStart ?? e.target.value.length);
                  setComment(e.target.value);
                }}
                onClick={(e) => setCaret(e.currentTarget.selectionStart ?? e.currentTarget.value.length)}
                onKeyUp={(e) => setCaret(e.currentTarget.selectionStart ?? e.currentTarget.value.length)}
                onSelect={(e) => setCaret(e.currentTarget.selectionStart ?? e.currentTarget.value.length)}
                onKeyDown={(e) => {
                  if (e.nativeEvent.isComposing || e.nativeEvent.keyCode === 229) return;
                  if (mentionMatches.length === 0 || !activeMention) return;
                  if (e.key === "ArrowDown" && selectableIndices.length > 0) {
                    e.preventDefault();
                    const cur = selectableIndices.indexOf(mentionHiClamped);
                    setMentionHi(selectableIndices[cur < 0 ? 0 : Math.min(cur + 1, selectableIndices.length - 1)]!);
                    return;
                  }
                  if (e.key === "ArrowUp" && selectableIndices.length > 0) {
                    e.preventDefault();
                    const cur = selectableIndices.indexOf(mentionHiClamped);
                    setMentionHi(selectableIndices[cur <= 0 ? 0 : cur - 1]!);
                    return;
                  }
                  if (e.key === "Escape") {
                    e.preventDefault();
                    e.stopPropagation();
                    setMentionOffFor(mentionSpanKey);
                    return;
                  }
                  if (mentionHiClamped >= 0 && (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey))) {
                    e.preventDefault();
                    const picked = mentionMatches[mentionHiClamped];
                    if (picked) completeMention(picked);
                  }
                }}
                placeholder={t("Add an update or a question…")}
              />
            </div>
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
              {t(busy ? "Posting…" : "Post comment")}
            </Button>
          </div>
        </div>
      )}
    </section>
  );
}
