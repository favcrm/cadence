import { useWriteBlock } from "../auth/WriteGate";
import { useState } from "react";
import { api, ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import type { HomeNeed } from "./needs";
import Button from "../../ui/Button";

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
  const { issue, report, options, impact, body } = need.action;
  const block = useWriteBlock(readOnly);
  const [text, setText] = useState("");
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
  return (
    <div className="mt-2 space-y-2">
      {need.summary && <p className="text-label text-ink-300 break-words">{need.summary}</p>}
      {impact && <p className="text-micro text-ink-500 break-words">Impact: {impact}</p>}
      {body && (
        <details>
          <summary className="text-micro text-ink-500 cursor-pointer">the question in full</summary>
          <pre className="mt-1 whitespace-pre-wrap break-words text-micro text-ink-400">{body}</pre>
        </details>
      )}
      {readOnly ? (
        <p className="text-micro text-ink-500">{block} Or answer with `cadence report file --kind answer`.</p>
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
            aria-label={`answer ${issue}`}
          />
          <Button variant="primary" size="sm" disabled={busy} loading={busy} onClick={() => send(text)}>
            {busy ? "Filing…" : "Send answer"}
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

