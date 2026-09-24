import { useState } from "react";
import { api, ApiError } from "../../lib/api";
import type { ResourceState } from "../../lib/cache";
import { resources } from "../../lib/resources";
import type { Overview } from "../../lib/types";
import { ageLabel, homeNeeds, type HomeNeed } from "./needs";
import PlanCard from "./PlanCard";
import Link from "../../ui/Link";

const KIND_CHIP: Record<string, string> = {
  plan: "bg-warn/10 text-warn",
  question: "bg-info/10 text-info",
  approval: "bg-warn/10 text-warn",
  merge: "bg-ok/15 text-ok",
};

function copy(text: string): Promise<void> {
  try {
    return navigator.clipboard.writeText(text);
  } catch (e) {
    return Promise.reject(e);
  }
}

/** The one next action of a question: answer it, as the operator. */
function AnswerForm({
  need,
  readOnly,
  onDone,
}: {
  need: HomeNeed & { action: { type: "answer" } };
  readOnly: boolean;
  onDone: (text: string) => void;
}) {
  const { issue, report, options, impact, body } = need.action;
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
        <p className="text-micro text-ink-500">Board is read-only — answer with `cadence report file --kind answer`.</p>
      ) : (
        <>
          {options.length > 0 && (
            <div className="flex flex-wrap gap-1.5">
              {options.map((o) => (
                <button
                  key={o}
                  disabled={busy}
                  onClick={() => send(o)}
                  className="h-8 px-2.5 rounded border border-ink-600 text-label text-ink-200 hover:border-accent/60 hover:text-accent disabled:opacity-40 max-w-full truncate"
                  title={`answer: ${o}`}
                >
                  {o}
                </button>
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
          <button
            disabled={busy}
            onClick={() => send(text)}
            className="h-8 px-3 rounded bg-accent text-on-accent text-label font-medium disabled:opacity-40"
          >
            {busy ? "Filing…" : "Send answer"}
          </button>
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

function NeedItem({
  need,
  readOnly,
  onOpenIssue,
}: {
  need: HomeNeed;
  readOnly: boolean;
  onOpenIssue: (id: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [done, setDone] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const action = need.action;
  const label =
    action.type === "plan" ? "Review plan" : action.type === "answer" ? "Answer" : copied ? "Copied" : "Copy command";
  return (
    <li className="px-3 py-2.5 min-w-0" data-need={need.kind}>
      <div className="flex items-start gap-2 min-w-0">
        <span className={`chip shrink-0 ${KIND_CHIP[need.kind] ?? "bg-ink-800 text-ink-300"}`}>{need.label}</span>
        <div className="min-w-0 flex-1">
          <div className="text-secondary text-ink-200 break-words">{need.title}</div>
          <div className="text-micro text-ink-500 mt-0.5">
            {need.owner} · waiting {ageLabel(need.age)}
          </div>
        </div>
      </div>
      {done ? (
        <p className="text-micro text-ok mt-1.5">answered: {done}</p>
      ) : (
        <button
          className="mt-1.5 text-label lnk"
          aria-expanded={action.type === "command" ? undefined : open}
          onClick={() => {
            if (action.type === "command") {
              copy(action.command).then(
                () => {
                  setCopied(true);
                  setTimeout(() => setCopied(false), 1500);
                },
                () => setOpen((o) => !o),
              );
            } else setOpen((o) => !o);
          }}
        >
          {label}
          {action.type !== "command" && (open ? " ▴" : " ▾")}
        </button>
      )}
      {open && action.type === "command" && (
        <pre className="mt-1 whitespace-pre-wrap break-all text-micro text-ink-400">{action.command}</pre>
      )}
      {open && action.type === "plan" && (
        <div className="mt-2">
          <PlanCard epic={action.epic} readOnly={readOnly} onOpenIssue={onOpenIssue} />
        </div>
      )}
      {open && action.type === "answer" && !done && (
        <AnswerForm
          need={need as HomeNeed & { action: { type: "answer" } }}
          readOnly={readOnly}
          onDone={(t) => {
            setDone(t);
            setOpen(false);
          }}
        />
      )}
    </li>
  );
}

/** "Needs you" — built from the overview's `needs_me` (CAD-328). */
export default function NeedsRail({
  overview,
  readOnly,
  onOpenIssue,
  overviewHref,
}: {
  overview: ResourceState<Overview>;
  readOnly: boolean;
  onOpenIssue: (id: string) => void;
  overviewHref: string;
}) {
  const needs = homeNeeds(overview.data?.needs_me);
  return (
    <section className="card overflow-hidden" aria-label="needs you">
      <header className="px-3 py-2.5 flex items-center gap-2 border-b border-ink-700">
        <h2 className="text-secondary font-semibold text-ink-100">Needs you</h2>
        <span className={`chip ${needs.length ? "bg-warn/10 text-warn" : "bg-ok/15 text-ok"}`}>
          {overview.data ? needs.length || "clear" : "…"}
        </span>
        <Link href={overviewHref} className="lnk text-label ml-auto">
          Team overview
        </Link>
      </header>
      {!overview.data && overview.status === "failed" && (
        <p className="px-3 py-2.5 text-label text-fail break-words">Overview unavailable — {overview.error}</p>
      )}
      {!overview.data && overview.status !== "failed" && (
        <p className="px-3 py-2.5 text-label text-ink-500">Reading what needs you…</p>
      )}
      {overview.data && needs.length === 0 && (
        <p className="px-3 py-2.5 text-label text-ink-500">Nothing needs a decision. The team is working.</p>
      )}
      {needs.length > 0 && (
        <ul className="divide-y divide-ink-700">
          {needs.map((n) => (
            <NeedItem key={n.key} need={n} readOnly={readOnly} onOpenIssue={onOpenIssue} />
          ))}
        </ul>
      )}
    </section>
  );
}
