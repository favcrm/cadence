import { useState } from "react";
import { useWriteBlock } from "../auth/WriteGate";
import type { ResourceState } from "../../lib/cache";
import type { Agent, Overview } from "../../lib/types";
import { kindSpec, metaLine, todoSplit, updateTitle, type HomeNeed } from "./needs";
import AgentUpdates from "./AgentUpdates";
import type { AgentUpdate } from "./agentUpdateModel";
import { READ_ONLY_COPY } from "./AnswerForm";
import ReviewDrawer from "./ReviewDrawer";
import TodoCard from "./TodoCard";
import Link from "../../ui/Link";

export { NeedMenu } from "./NeedMenu";

/**
 * The Home rail (CAD-574, CAD-1216): "To do" holds only work for the
 * operator, one line per card with one control; "Updates" holds the
 * informational rows and the team's recent work. It collapses to a slim
 * rail with the count (persisted per viewer) and, under ~1100px, is a
 * slide-over opened from the floating button. Reading items (plans,
 * ideas, merge decisions) open the review drawer from the right edge;
 * the count and the sidebar's Home badge include pending items only.
 */
export default function NeedsRail({
  overview,
  readOnly,
  onOpenIssue,
  overviewHref,
  permissionsHref,
  onAsk,
  collapsed,
  onToggleCollapse,
  onAskAgent,
}: {
  overview: ResourceState<Overview>;
  readOnly: boolean;
  onOpenIssue: (id: string) => void;
  overviewHref: string;
  /** Settings → Master permissions, where the standing "Always" rules live. */
  permissionsHref: string;
  onAsk: (need: HomeNeed, lead?: string) => void;
  collapsed: boolean;
  onToggleCollapse: () => void;
  onAskAgent: (agent: Agent, update?: AgentUpdate) => void;
}) {
  const [tab, setTab] = useState<"todo" | "updates">("todo");
  const block = useWriteBlock(readOnly);
  const { todo, updates, decided } = todoSplit(overview.data?.needs_me);
  const [drawer, setDrawer] = useState(false);
  const [hidden, setHidden] = useState<ReadonlySet<string>>(() => new Set());
  const [done, setDone] = useState<ReadonlyMap<string, string>>(() => new Map());
  const [review, setReview] = useState<string | null>(null);
  const [showDecided, setShowDecided] = useState(false);

  const cards = todo.filter((n) => !hidden.has(n.key));
  const count = cards.filter((n) => !done.has(n.key)).length;
  const readers = cards.filter((n) => kindSpec(n).place === "drawer");
  const reading = review === null ? -1 : readers.findIndex((n) => n.key === review);
  const firstPermission = cards.findIndex((n) => !done.has(n.key) && n.kind === "master_permission");

  const hide = (key: string) => setHidden((s) => new Set(s).add(key));
  const markDone = (key: string, text: string) => setDone((m) => new Map(m).set(key, text));
  /** A review decision landed: mark the card, then move on to the next reading item. */
  const reviewed = (key: string, text: string) => {
    markDone(key, text);
    const next = readers.slice(readers.findIndex((n) => n.key === key) + 1).find((n) => !done.has(n.key));
    setReview(next ? next.key : null);
  };
  const ask = (need: HomeNeed, lead?: string) => {
    setDrawer(false);
    onAsk(need, lead);
  };
  const closeReview = () => {
    const key = review;
    setReview(null);
    if (key === null) return;
    // Hand focus back to the card that opened the drawer (or the nearest one that is left).
    setTimeout(() => {
      const target = Array.from(document.querySelectorAll<HTMLElement>("[data-need-key]"))
        .filter((el) => el.getAttribute("data-need-key") === key)
        .map((el) => el.querySelector<HTMLElement>(".todo-hit, button"))
        .find((el) => el !== null && el.isConnected);
      target?.focus();
    }, 30);
  };

  const todoBody = (
    <>
      {block && <p className="todo-note">{READ_ONLY_COPY}</p>}
      {!overview.data && overview.status === "failed" && (
        <p className="px-3 py-2.5 text-label text-fail break-words">The list couldn't be loaded. It will try again.</p>
      )}
      {!overview.data && overview.status !== "failed" && (
        <p className="px-3 py-2.5 text-label text-ink-500">Reading what needs you…</p>
      )}
      {overview.data && cards.length === 0 && (
        <p className="px-3 py-3 text-label text-ink-400">Nothing needs you. The team is working.</p>
      )}
      <ul className="todo-list">
        {cards.map((n, i) => (
          <TodoCard
            key={n.key}
            need={n}
            readOnly={readOnly}
            defaultOpen={i === firstPermission}
            doneText={done.get(n.key)}
            onReview={setReview}
            onOpenIssue={onOpenIssue}
            onAsk={(need) => ask(need)}
            onHide={hide}
            onDone={markDone}
            onSent={() => setDrawer(false)}
          />
        ))}
      </ul>
      {cards.length > 0 && <p className="todo-hint">Tap a card to see more. “Fix it” hands the problem to Master.</p>}
      {decided.length > 0 && (
        <section className="todo-decided" data-todo-decided>
          <button type="button" className="todo-decided-h" aria-expanded={showDecided} onClick={() => setShowDecided((s) => !s)}>
            <span className="steps-caret num" aria-hidden>
              ›
            </span>
            Recently decided ({decided.length})
          </button>
          {showDecided && (
            <>
              <ul>
                {decided.map((n) => (
                  <li key={n.key}>
                    <span className="todo-decided-t">{kindSpec(n).title(n, { issueTitle: null })}</span>
                    <span className="chip bg-ink-800 text-ink-300 shrink-0">
                      {n.action.type === "permission" ? n.action.decisionLabel || n.action.status : "decided"}
                    </span>
                  </li>
                ))}
              </ul>
              <Link href={permissionsHref} className="lnk text-label" onClick={() => setDrawer(false)}>
                Standing rules are in Settings
              </Link>
            </>
          )}
        </section>
      )}
    </>
  );

  const updatesBody = (
    <>
      {updates.length > 0 && (
        <ul className="todo-fyi" data-todo-updates>
          {updates.map((n) => (
            <li key={n.key}>
              <span>{updateTitle(n)}</span>
              <span className="text-ink-500">{metaLine(n)}</span>
            </li>
          ))}
        </ul>
      )}
      <AgentUpdates
        onAsk={(agent, update) => {
          setDrawer(false);
          onAskAgent(agent, update);
        }}
        onOpenIssue={onOpenIssue}
      />
    </>
  );

  const tabs = (drawerMode: boolean) => (
    <div className="todo-tabs" role="group" aria-label="Home rail">
      <button aria-pressed={tab === "todo"} onClick={() => setTab("todo")}>
        To do <span className={`todo-count ${count ? "on" : ""}`}>{overview.data ? count : "…"}</span>
      </button>
      <button aria-pressed={tab === "updates"} onClick={() => setTab("updates")}>
        Updates
      </button>
      <span className="todo-tools">
        <Link href={overviewHref} className="lnk text-label" onClick={() => setDrawer(false)}>
          Team overview
        </Link>
        {drawerMode ? (
          <button type="button" className="lnk text-label" aria-label="close the rail" onClick={() => setDrawer(false)}>
            ✕
          </button>
        ) : (
          <button
            type="button"
            className="lnk text-label"
            aria-label={collapsed ? "expand the rail" : "collapse the rail"}
            onClick={onToggleCollapse}
          >
            {collapsed ? "»" : "«"}
          </button>
        )}
      </span>
    </div>
  );

  const panelBody = <div>{tab === "todo" ? todoBody : updatesBody}</div>;

  return (
    <>
      {/* ≥1100px: the detached column — sticky in the grid, scrolling on
          its own; collapsed it is the slim rail with the count. */}
      <div className="hidden rail:block h-full min-h-0">
        {collapsed ? (
          <button
            type="button"
            className="slimrail card"
            aria-label={`to do — ${count} waiting; expand the rail`}
            onClick={onToggleCollapse}
          >
            <span className={`chip ${count ? "bg-warn/10 text-warn" : "bg-ok/15 text-ok"}`}>
              {overview.data ? count : "…"}
            </span>
            <span className="vert text-label text-ink-400">To do</span>
            <span className="text-ink-500" aria-hidden>
              «
            </span>
          </button>
        ) : (
          <section className="card needsrail overflow-hidden" aria-label="to do" data-collapsed="false">
            {tabs(false)}
            <div className="rail-scroll min-h-0">{panelBody}</div>
          </section>
        )}
      </div>

      {/* <1100px: a floating button opens the slide-over drawer. */}
      <button
        type="button"
        className="needbtn rail:hidden"
        onClick={() => setDrawer(true)}
        aria-label={`to do — ${count} waiting; open the list`}
      >
        To do
        <span className={`chip ${count ? "bg-warn/15 text-warn" : "bg-ok/15 text-ok"}`}>
          {overview.data ? count : "…"}
        </span>
      </button>
      {drawer && (
        <>
          <div className="needsscrim rail:hidden" onClick={() => setDrawer(false)} />
          <section className="needsdrawer rail:hidden" role="dialog" aria-label="to do" aria-modal="true">
            {tabs(true)}
            <div className="rail-scroll min-h-0 flex-1">{panelBody}</div>
          </section>
        </>
      )}

      {reading >= 0 && (
        <ReviewDrawer
          key={readers[reading].key}
          need={readers[reading]}
          readOnly={readOnly}
          index={reading + 1}
          total={readers.length}
          onDone={reviewed}
          onClose={closeReview}
          onAsk={(need, lead) => ask(need, lead)}
        />
      )}
    </>
  );
}
