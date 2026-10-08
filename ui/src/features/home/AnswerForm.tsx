import { useWriteBlock } from "../auth/WriteGate";
import { useState } from "react";
import { api, ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import type { HomeNeed } from "./needs";
import Button from "../../ui/Button";

/** What a blocked action says: one plain line, no command. */
export const READ_ONLY_COPY = "You're viewing read-only.";

/** Answer a question as the operator: one send, shared by the form and the card's Yes/No. */
export function useAnswer(
  need: HomeNeed & { action: { type: "answer" } },
  onDone: (text: string) => void,
) {
  const { issue, report } = need.action;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const send = (answer: string) => {
    if (!answer.trim()) {
      setError("Write an answer or pick an option.");
      return;
    }
    setBusy(true);
    setError(null);
    api
      .answer(issue, report, answer)
      .then(() => {
        void resources.overview.invalidate();
        void resources.issue(issue).invalidate();
        onDone(answer);
      })
      .catch((e: ApiError) => setError(e.message ?? String(e)))
      .finally(() => setBusy(false));
  };
  return { send, busy, error };
}

/** The one next action of a question: answer it, as the operator. */
export default function AnswerForm({
  need,
  readOnly,
  onDone,
}: {
  need: HomeNeed & { action: { type: "answer" } };
  readOnly: boolean;
  onDone: (text: string) => void;
}) {
  const { options, impact, body } = need.action;
  const block = useWriteBlock(readOnly);
  const [text, setText] = useState("");
  const { send, busy, error } = useAnswer(need, onDone);
  return (
    <div className="mt-2 space-y-2">
      {/* The card's title already carries the first line of the summary. */}
      {need.summary && (need.summary.length > 90 || need.summary.includes("\n")) && (
        <p className="text-label text-ink-300 break-words line-clamp-2">{need.summary}</p>
      )}
      {(body || impact) && (
        <details>
          <summary className="text-micro text-ink-500 cursor-pointer">the question in full</summary>
          {impact && <p className="mt-1 text-micro text-ink-500 break-words">Impact: {impact}</p>}
          {body && <pre className="mt-1 whitespace-pre-wrap break-words text-micro text-ink-400">{body}</pre>}
        </details>
      )}
      {block ? (
        <p className="text-micro text-ink-500">{READ_ONLY_COPY}</p>
      ) : (
        <>
          {options.length > 0 && (
            <div className="flex flex-wrap gap-1.5">
              {options.map((o) => (
                <Button key={o} size="sm" disabled={busy} onClick={() => send(o)} title={`answer: ${o}`}>
                  {o}
                </Button>
              ))}
            </div>
          )}
          <textarea
            value={text}
            onChange={(e) => setText(e.target.value)}
            rows={2}
            className="field w-full text-secondary"
            placeholder="Or write an answer…"
            aria-label="your answer"
          />
          <Button variant="primary" size="sm" disabled={busy} loading={busy} onClick={() => send(text)}>
            {busy ? "Sending…" : "Send answer"}
          </Button>
        </>
      )}
      {error && (
        <p className="text-micro text-fail break-words" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
