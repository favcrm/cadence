import { useEffect, useState, type KeyboardEvent, type ReactNode, type RefCallback } from "react";
import { useWriteBlock } from "../auth/WriteGate";
import { api } from "../../lib/api";
import { resources } from "../../lib/resources";
import { useMaybeResource } from "../../lib/useResource";
import AnswerForm, { READ_ONLY_COPY, useAnswer } from "./AnswerForm";
import { NeedMenu } from "./NeedMenu";
import PermissionCard from "./PermissionCard";
import {
  fixPrompt,
  kindSpec,
  metaLine,
  needRefs,
  plainFailure,
  UNFENCE_CHOICES,
  type HomeNeed,
  type NeedType,
  type UnfenceChoice,
} from "./needs";
import { sendToMaster } from "./send";
import { marksFor, sendingTodo, settleTodo, useTodoLocal } from "./todoLocal";

const ICON: Record<string, string[]> = {
  list: ["M9 5h10M9 12h10M9 19h10", "M4 5l1 1 2-2M4 12l1 1 2-2M4 19l1 1 2-2"],
  globe: ["M12 21a9 9 0 100-18 9 9 0 000 18z", "M3 12h18M12 3a14 14 0 010 18M12 3a14 14 0 000 18"],
  shield: ["M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6z", "M9 12l2 2 4-4"],
  chat: ["M21 12a8 8 0 01-11.6 7.1L4 20l1-4.6A8 8 0 1121 12z"],
  warn: ["M12 3l9 16H3z", "M12 10v4M12 17h.01"],
};

function iconFor(need: HomeNeed, type: NeedType): string[] {
  if (type === "question") return ICON.chat;
  if (type === "stuck") return ICON.warn;
  if (need.kind === "plan" || need.kind === "idea_plan") return ICON.list;
  if (need.kind === "merge_decision") return ICON.globe;
  return ICON.shield;
}

/** The ticket title behind a plan, idea or merge row, once the issue is read. */
function useIssueTitle(need: HomeNeed): string | null {
  const a = need.action;
  const id = a.type === "plan" ? a.epic : a.type === "idea" || a.type === "merge" ? a.issue : null;
  const store = id ? resources.issue(id) : null;
  useEffect(() => {
    if (store) void store.revalidate();
  }, [store]);
  const state = useMaybeResource(store);
  return state?.data && state.data.id === id ? state.data.title : null;
}

/** The technical fields the card leaves out — one tap away under ⋯ → Details. */
function Details({ need, failure }: { need: HomeNeed; failure: string | null }) {
  const a = need.action;
  const rows: [string, string | null][] = [
    ["kind", need.kind],
    ["issue", need.issue],
    ["pr", a.type === "merge" ? a.pr : null],
    ["head", a.type === "merge" ? a.sha : null],
    ["reviewer", a.type === "merge" ? a.reviewer : null],
    ["agent", need.agent],
    ["owner", need.owner],
    ["command", a.type === "permission" ? a.argv : need.command],
    ["folder", a.type === "permission" && a.cwd ? a.cwd : null],
    ["risk", a.type === "permission" ? a.risk : null],
    ["row", need.title],
    ["last error", failure],
  ];
  return (
    <dl className="todo-tech" data-todo-details>
      {rows
        .filter((r): r is [string, string] => r[1] !== null && r[1] !== "")
        .map(([k, v]) => (
          <div key={k}>
            <dt>{k}</dt>
            <dd>{v}</dd>
          </div>
        ))}
    </dl>
  );
}

/** Shorten a Yes/No label so two of them fit a one-line card. */
const shortOption = (o: string) => (o.length > 14 ? `${o.slice(0, 13).trimEnd()}…` : o);

/** A question with exactly two options: both are one tap, answering as the operator. */
function YesNo({
  need,
  block,
  onDone,
  onError,
}: {
  need: HomeNeed & { action: { type: "answer" } };
  block: string | null;
  onDone: (text: string) => void;
  onError: (message: string | null) => void;
}) {
  const { send, busy, error } = useAnswer(need, onDone);
  useEffect(() => onError(error), [error, onError]);
  return (
    <span className="todo-yn" role="group" aria-label="your answer">
      {need.action.options.map((o) => (
        <button
          key={o}
          type="button"
          className="btn btn-sm"
          disabled={!!block || busy}
          title={block ? READ_ONLY_COPY : `Answer: ${o}`}
          onClick={() => send(o)}
        >
          {shortOption(o)}
        </button>
      ))}
    </span>
  );
}

/**
 * One To do card (CAD-1216): an icon whose colour gives the type, a plain
 * one-line title, one meta line and at most one control. Quick items open
 * in place; reading items open the review drawer; stuck items hand the
 * problem to Master ("Fix it"). Technical fields stay under ⋯ → Details.
 */
export default function TodoCard({
  need,
  readOnly,
  defaultOpen,
  doneText,
  onReview,
  onOpenIssue,
  onAsk,
  onHide,
  onDone,
  onSent,
  hitRef,
}: {
  need: HomeNeed;
  readOnly: boolean;
  defaultOpen: boolean;
  /** What the operator did, once they did it — the card greys out. */
  doneText: string | undefined;
  onReview: (key: string) => void;
  onOpenIssue: (id: string) => void;
  onAsk: (need: HomeNeed) => void;
  onHide: (need: HomeNeed) => void;
  onDone: (need: HomeNeed, text: string) => void;
  /** Fired after "Fix it" sends, so a slide-over can make room for the chat. */
  onSent: () => void;
  /** The card's tap target, so the review drawer can hand focus back to it. */
  hitRef: RefCallback<HTMLElement>;
}) {
  const block = useWriteBlock(readOnly);
  const spec = kindSpec(need);
  const issueTitle = useIssueTitle(need);
  // The kind table's template is the fallback; "Fix it" always sends the
  // template, never the row's agent-authored short_title as an instruction.
  const template = spec.title(need, { issueTitle });
  const title = need.shortTitle ?? template;
  const [open, setOpen] = useState(defaultOpen && spec.place === "inline");
  const [menu, setMenu] = useState(false);
  const [details, setDetails] = useState(false);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // The server's own words for the last failure: under ⋯ → Details only, never on the card.
  const [failure, setFailure] = useState<string | null>(null);
  // Kept outside the card: a slide-over that remounts it must not offer "Fix it" again.
  const { sent, sending } = marksFor(useTodoLocal(), need);
  const action = need.action;
  const answer = action.type === "answer" ? (need as HomeNeed & { action: { type: "answer" } }) : null;
  const yesNo = answer && answer.action.options.length === 2 ? answer : null;
  const done = doneText !== undefined;
  const stopped = need.kind === "stopped" && !!need.agent;
  const expandable = spec.place === "inline" || stopped;
  const activates = expandable || spec.place === "drawer";

  const refresh = () => {
    void resources.overview.invalidate();
    void resources.agents.invalidate();
  };
  const run = (work: () => Promise<unknown>, what: string, hide: boolean) => {
    setBusy(true);
    setError(null);
    work()
      .then(() => {
        setMenu(false);
        if (hide) onHide(need);
        else onDone(need, what);
        refresh();
      })
      .catch((e: unknown) => {
        setError(plainFailure(e));
        setFailure((e as Error)?.message ?? null);
      })
      .finally(() => setBusy(false));
  };
  const decide = (verb: "snooze" | "dismiss", secs?: number) => {
    const id = need.subject ?? { kind: "row", id: need.key };
    run(() => api.needDecide(verb, id.kind, id.id, secs), "", true);
  };
  const agentAct = (verb: "resume" | "unfence", status?: UnfenceChoice["status"]) => {
    if (!need.agent) return;
    run(
      () => api.agentAct(need.agent!, verb, status),
      verb === "resume" ? "Resumed" : `Recorded: ${UNFENCE_CHOICES.find((c) => c.status === status)?.plain ?? status}`,
      false,
    );
  };
  const fixIt = () => {
    sendingTodo(need);
    setError(null);
    void sendToMaster(fixPrompt(need, template), undefined, needRefs(need)).then((r) => {
      settleTodo(need, r.ok);
      if (r.ok) {
        onSent();
      } else {
        setError("It couldn't be sent to Master. Try again.");
        setFailure(r.error);
      }
    });
  };
  const control = () => {
    if (spec.place === "drawer") onReview(need.key);
    else if (spec.place === "send") fixIt();
    else setOpen((o) => !o);
  };
  const activate = () => {
    if (done || !activates) return;
    if (spec.place === "drawer") onReview(need.key);
    else setOpen((o) => !o);
  };
  const onKey = (e: KeyboardEvent) => {
    if (e.key !== "Enter" && e.key !== " ") return;
    e.preventDefault();
    activate();
  };

  const ask = (
    <button type="button" className="btn btn-sm btn-ghost" disabled={!!block} title={block ? READ_ONLY_COPY : undefined} onClick={() => onAsk(need)}>
      Ask Master
    </button>
  );

  let inside: ReactNode = null;
  if (open && !done) {
    if (action.type === "permission") {
      inside = (
        <PermissionCard
          compact
          // No stated risk shows no risk line ("low" is the card's quiet state).
          card={action.risk ? action : { ...action, risk: "low" }}
          readOnly={readOnly}
          onDone={(t) => onDone(need, t)}
        />
      );
    } else if (answer) {
      inside = <AnswerForm need={answer} readOnly={readOnly} onDone={(t) => onDone(need, `Answered: ${t}`)} />;
    } else if (need.kind === "fenced" && need.agent) {
      inside = (
        <div className="mt-2 space-y-2">
          <p className="text-label text-ink-400">How did its last piece of work end?</p>
          <div className="flex flex-wrap gap-1.5" role="group" aria-label="how the work ended">
            {UNFENCE_CHOICES.map((c) => (
              <button
                key={c.status}
                type="button"
                className="btn btn-sm"
                disabled={!!block || busy}
                title={block ? READ_ONLY_COPY : c.blurb}
                onClick={() => agentAct("unfence", c.status)}
              >
                {c.plain}
              </button>
            ))}
          </div>
          {block && <p className="text-micro text-ink-500">{READ_ONLY_COPY}</p>}
        </div>
      );
    } else if (stopped) {
      inside = (
        <div className="mt-2 space-y-2">
          <p className="text-label text-ink-400">It stopped with work still waiting.</p>
          <div className="flex flex-wrap gap-1.5">
            <button type="button" className="btn btn-sm btn-primary" disabled={!!block || busy} title={block ? READ_ONLY_COPY : undefined} onClick={() => agentAct("resume")}>
              {busy ? "Working…" : "Start it again"}
            </button>
          </div>
          {block && <p className="text-micro text-ink-500">{READ_ONLY_COPY}</p>}
        </div>
      );
    } else {
      inside = (
        <div className="mt-2 space-y-2">
          <p className="text-label text-ink-400">Master can walk you through this and take it from there.</p>
          <div className="flex flex-wrap gap-1.5">{ask}</div>
        </div>
      );
    }
  }

  const quickRow =
    (action.type === "permission" || answer) && open ? (
      <div className="mt-1 flex flex-wrap gap-1">
        <button type="button" className="btn btn-sm btn-ghost" onClick={() => setOpen(false)}>
          Not now
        </button>
        {ask}
      </div>
    ) : null;
  const showControl = !done && !(open && spec.place === "inline");
  const controlButton = yesNo ? (
    <YesNo need={yesNo} block={block} onDone={(t) => onDone(need, `Answered: ${t}`)} onError={setError} />
  ) : sent ? (
    <span className="text-micro text-ok">Sent to Master</span>
  ) : (
    <button
      type="button"
      className={`btn btn-sm ${spec.control === "Allow" ? "btn-primary" : ""}`}
      aria-expanded={spec.place === "inline" ? open : undefined}
      disabled={spec.place !== "drawer" && (!!block || sending)}
      title={spec.place !== "drawer" && block ? READ_ONLY_COPY : undefined}
      onClick={control}
    >
      {spec.control}
    </button>
  );

  return (
    <li className="needrow todo" data-need={need.kind} data-need-key={need.key} data-type={spec.type} data-open={open || undefined} data-done={done || undefined}>
      <div className="todo-card">
        <span className="todo-icon" aria-hidden>
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            {iconFor(need, spec.type).map((d) => (
              <path key={d} d={d} />
            ))}
          </svg>
        </span>
        <div className="todo-main min-w-0">
          <div
            className="todo-hit"
            role={activates && !done ? "button" : undefined}
            tabIndex={activates && !done ? 0 : undefined}
            aria-expanded={expandable && !done ? open : undefined}
            ref={hitRef}
            onClick={activate}
            onKeyDown={onKey}
          >
            <div className="todo-title" title={title}>
              {title}
            </div>
            <div className="todo-meta">{done ? doneText : metaLine(need)}</div>
          </div>
          {inside}
          {!done && quickRow}
          {error ? (
            <p className="text-micro text-fail mt-1 break-words" role="alert">
              {error}
            </p>
          ) : null}
          {note && <p className="text-micro text-ok mt-1">{note}</p>}
        </div>
        <div className="todo-act">
          {showControl && controlButton}
          {!done && (
            <button type="button" className="needmore" aria-label="more actions" aria-expanded={menu} onClick={() => setMenu((m) => !m)}>
              ⋯
            </button>
          )}
        </div>
      </div>
      {details && <Details need={need} failure={failure} />}
      {menu && (
        <NeedMenu
          need={need}
          block={block ? READ_ONLY_COPY : null}
          busy={busy}
          onOpenIssue={(id) => {
            setMenu(false);
            onOpenIssue(id);
          }}
          onDecide={decide}
          onCopied={() => {
            setNote("Copied");
            setTimeout(() => setNote(null), 1500);
          }}
          onClose={() => setMenu(false)}
          onDetails={() => setDetails((d) => !d)}
        />
      )}
    </li>
  );
}
