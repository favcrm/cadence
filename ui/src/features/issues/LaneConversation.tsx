import { useContext, useEffect, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { WriteGate } from "../auth/WriteGate";
import { asEntry, type ThreadEntry } from "../home/thread";
import Button from "../../ui/Button";
import Md from "../../ui/Md";
import { composerBlocked, isStatusAsk, statusText } from "./lane";

/** The lane agent's thread, with Ask for status and Instruction composers. */
export function LaneConversation({
  issue,
  agent,
  state,
  mode,
  onMode,
}: {
  issue: string;
  agent: string | null;
  state: string | null;
  mode?: "ask" | "instruct";
  onMode?: (mode: "ask" | "instruct") => void;
}) {
  const writeBlock = useContext(WriteGate);
  const [entries, setEntries] = useState<ThreadEntry[]>([]);
  const [draft, setDraft] = useState("");
  const [localMode, setLocalMode] = useState<"ask" | "instruct">(mode ?? "ask");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const active = mode ?? localMode;
  const blocked = composerBlocked(state) || writeBlock != null || !agent;

  useEffect(() => {
    if (mode) setLocalMode(mode);
  }, [mode]);

  useEffect(() => {
    if (!agent) {
      setEntries([]);
      return;
    }
    let cancel = false;
    api.thread(agent, { tail: true, limit: 200 }).then(
      (page) => {
        if (cancel) return;
        setEntries(page.entries.map(asEntry).filter((e): e is ThreadEntry => e != null));
      },
      () => {
        if (!cancel) setEntries([]);
      },
    );
    return () => {
      cancel = true;
    };
  }, [agent]);

  const setMode = (next: "ask" | "instruct") => {
    setLocalMode(next);
    onMode?.(next);
  };

  const send = async () => {
    if (!agent || blocked) return;
    setBusy(true);
    setError(null);
    try {
      if (active === "ask") {
        const extra = draft.trim();
        await api.laneAsk(issue, extra || undefined);
      } else {
        const text = draft.trim();
        if (!text) {
          setError("an instruction needs text");
          return;
        }
        await api.laneInstruct(issue, text);
      }
      setDraft("");
      const page = await api.thread(agent, { tail: true, limit: 200 });
      setEntries(page.entries.map(asEntry).filter((e): e is ThreadEntry => e != null));
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "the message was not sent");
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="card" data-testid="lane-conversation">
      <h2 className="text-ink">Conversation</h2>
      {!agent && <p data-testid="lane-thread-empty">No lane yet</p>}
      <ol data-testid="lane-thread">
        {entries.map((entry) => {
          const ask = entry.role === "operator" && isStatusAsk(entry.payload?.source, entry.text);
          return (
            <li key={entry.seq} data-testid="lane-entry">
              <span className="chip">{entry.role === "operator" ? "you" : entry.role}</span>
              {ask && <span className="chip" data-testid="lane-status-badge">status</span>}
              <Md text={entry.text} />
            </li>
          );
        })}
      </ol>
      <div className="flex gap-2">
        <span data-testid="lane-mode-ask" className="inline-flex">
          {/* CAD-617 Button does not forward aria-pressed. */}
          <Button
            size="sm"
            variant={active === "ask" ? "primary" : "ghost"}
            aria-label="Ask for status"
            onClick={() => setMode("ask")}
          >
            Ask for status
          </Button>
        </span>
        <span data-testid="lane-mode-instruct" className="inline-flex">
          <Button
            size="sm"
            variant={active === "instruct" ? "primary" : "ghost"}
            aria-label="Instruction"
            onClick={() => setMode("instruct")}
          >
            Instruction
          </Button>
        </span>
      </div>
      <p className="text-sm" data-testid="lane-composer-hint">
        {active === "ask"
          ? "Light nudge. One turn, then the answer lands in this thread."
          : "A real operator message. The agent answers in this thread."}
      </p>
      {active === "ask" && (
        <p className="num" data-testid="lane-status-preview">{statusText(draft)}</p>
      )}
      <textarea
        data-testid="lane-composer"
        value={draft}
        disabled={blocked || busy}
        onChange={(e) => setDraft(e.target.value)}
      />
      {error && <p className="text-fail" data-testid="lane-composer-error">{error}</p>}
      <span data-testid="lane-send" className="inline-flex">
        <Button
          size="sm"
          variant="primary"
          loading={busy}
          disabled={blocked}
          onClick={() => void send()}
        >
          Send
        </Button>
      </span>
    </section>
  );
}
