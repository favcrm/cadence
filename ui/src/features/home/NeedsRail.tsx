import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useWriteBlock } from "../auth/WriteGate";
import type { ResourceState } from "../../lib/cache";
import type { Agent, Overview } from "../../lib/types";
import { kindSpec, metaLine, todoSplit, updateTitle, type HomeNeed } from "./needs";
import AgentUpdates from "./AgentUpdates";
import type { AgentUpdate } from "./agentUpdateModel";
import { useTranslate } from "../../lib/locale";
import { READ_ONLY_COPY } from "./AnswerForm";
import TodoCard from "./TodoCard";
import { decisionReviewHead, nextDecisionToken, type HomeDecisionOpen } from "../../lib/workItems";
import { beginTodoVisit, doneTodo, hideTodo, marksFor, useTodoLocal } from "./todoLocal";
import Link from "../../ui/Link";

export { NeedMenu } from "./NeedMenu";

/**
 * The Home rail (CAD-574, CAD-1216): "To do" holds only work for the
 * operator, one line per card with one control; "Updates" holds the
 * informational rows and the team's recent work. It collapses to a slim
 * rail with the count (persisted per viewer) and, under ~1100px, is a
 * slide-over opened from the floating button. Reading items (plans,
 * ideas, merge decisions) open inside the one shared work-item detail,
 * mounted by App (CAD-1811); the rail hands it the reading queue as it
 * stood when the item opened. The count and the sidebar's Home badge
 * include pending items only.
 */
export default function NeedsRail({
  overview,
  readOnly,
  onOpenIssue,
  overviewHref,
  permissionsHref,
  onAsk,
  onOpenDecision,
  collapsed,
  onToggleCollapse,
  onAskAgent,
  onSent,
}: {
  overview: ResourceState<Overview>;
  readOnly: boolean;
  onOpenIssue: (id: string) => void;
  overviewHref: string;
  /** Settings → Master permissions, where the standing "Always" rules live. */
  permissionsHref: string;
  onAsk: (need: HomeNeed, lead?: string) => void;
  /** Open a reading item in the shared detail (App owns the one mount). */
  onOpenDecision: (open: HomeDecisionOpen) => void;
  collapsed: boolean;
  onToggleCollapse: () => void;
  onAskAgent: (agent: Agent, update?: AgentUpdate) => void;
  /** A "Fix it" reached Master: Home pins the chat to the sent message. */
  onSent?: () => void;
}) {
  const [tab, setTab] = useState<"todo" | "updates">("todo");
  const block = useWriteBlock(readOnly);
  const t = useTranslate();
  const { todo, updates, decided } = todoSplit(overview.data?.needs_me);
  const [drawer, setDrawer] = useState(false);
  const local = useTodoLocal();
  // A new visit to Home: earlier decided and sent marks stop applying.
  useLayoutEffect(() => beginTodoVisit(), []);
  // The floating button that owns the slide-over's focus on the way out.
  const needBtn = useRef<HTMLButtonElement | null>(null);
  const drawerEl = useRef<HTMLElement | null>(null);
  const wasOpen = useRef(false);
  // The <1100px drawer is a dialog: it lands focus on itself, Escape
  // closes it, and closing hands focus back to the button that opened
  // it. An Ask hand-off is unaffected — the composer's own focus lands
  // after this one (child effects run before this parent's).
  useEffect(() => {
    if (!drawer) return;
    drawerEl.current?.focus();
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape") setDrawer(false);
    };
    addEventListener("keydown", onKey);
    return () => removeEventListener("keydown", onKey);
  }, [drawer]);
  useEffect(() => {
    if (!drawer && wasOpen.current) needBtn.current?.focus();
    wasOpen.current = drawer;
  }, [drawer]);
  // An issue opened from the slide-over leaves Home — drop the drawer so
  // a stale overlay never hangs over the navigation.
  const openIssue = (id: string) => {
    setDrawer(false);
    onOpenIssue(id);
  };
  const [showDecided, setShowDecided] = useState(false);

  const cards = todo.filter((n) => !marksFor(local, n).hidden);
  const isDone = (n: HomeNeed) => marksFor(local, n).doneText !== undefined;
  const count = cards.filter((n) => !isDone(n)).length;
  const readers = cards.filter((n) => kindSpec(n).place === "drawer");
  const firstPermission = cards.findIndex((n) => !isDone(n) && n.kind === "master_permission");
  const ask = (need: HomeNeed, lead?: string) => {
    setDrawer(false);
    onAsk(need, lead);
  };
  /** One reading item, with the queue as it stood when the detail opened:
   *  "n of m" does not recount when the overview refreshes mid-run. */
  const decisionFor = (need: HomeNeed, queue: HomeNeed[]): HomeDecisionOpen => ({
    need,
    index: Math.max(queue.findIndex((n) => n.key === need.key), 0) + 1,
    total: Math.max(queue.length, 1),
    token: nextDecisionToken(),
    reviewHead: decisionReviewHead(need),
    next: (text) => {
      doneTodo(need, text);
      const at = queue.findIndex((n) => n.key === need.key);
      const following = queue.slice(at + 1).find((n) => !isDone(n));
      return following ? decisionFor(following, queue) : null;
    },
    ask: (lead) => ask(need, lead),
  });
  const openDecision = (key: string) => {
    const need = readers.find((n) => n.key === key);
    if (!need) return;
    onOpenDecision(decisionFor(need, readers.filter((n) => n.key === key || !isDone(n))));
  };
  const todoBody = (
    <>
      {block && <p className="todo-note">{t(READ_ONLY_COPY)}</p>}
      {!overview.data && overview.status === "failed" && (
        <p className="px-3 py-2.5 text-label text-fail break-words">{t("The list couldn't be loaded. It will try again.")}</p>
      )}
      {!overview.data && overview.status !== "failed" && (
        <p className="px-3 py-2.5 text-label text-ink-500">{t("Reading what needs you…")}</p>
      )}
      {overview.data && cards.length === 0 && (
        <p className="px-3 py-3 text-label text-ink-400">{t("Nothing needs you. The team is working.")}</p>
      )}
      <ul className="todo-list">
        {cards.map((n, i) => (
          <TodoCard
            key={n.key}
            need={n}
            readOnly={readOnly}
            defaultOpen={i === firstPermission}
            doneText={marksFor(local, n).doneText}
            onReview={openDecision}
            onOpenIssue={openIssue}
            onAsk={(need) => ask(need)}
            onHide={hideTodo}
            onDone={doneTodo}
            onSent={() => {
              setDrawer(false);
              onSent?.();
            }}
          />
        ))}
      </ul>
      {cards.length > 0 && <p className="todo-hint">{t("Tap a card to see more. “Fix it” hands the problem to Master.")}</p>}
      {decided.length > 0 && (
        <section className="todo-decided" data-todo-decided>
          <button type="button" className="todo-decided-h" aria-expanded={showDecided} onClick={() => setShowDecided((s) => !s)}>
            <span className="steps-caret num" aria-hidden>
              ›
            </span>
            {t("Recently decided")} ({decided.length})
          </button>
          {showDecided && (
            <>
              <ul>
                {decided.map((n) => (
                  <li key={n.key}>
                    <span className="todo-decided-t">{n.why ?? n.shortTitle ?? kindSpec(n).title(n, { issueTitle: null })}</span>
                    <span className="chip bg-ink-800 text-ink-300 shrink-0">
                      {n.action.type === "permission" ? n.action.decisionLabel || n.action.status : "decided"}
                    </span>
                  </li>
                ))}
              </ul>
              <Link href={permissionsHref} className="lnk text-label" onClick={() => setDrawer(false)}>
                {t("Standing rules are in Settings")}
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
              <span className="text-ink-500">
                {/* An Updates row stays information; the subject id is a
                    plain link to the issue it names, never an action. */}
                {n.issue !== null && (
                  <button type="button" className="lnk num" onClick={() => openIssue(n.issue!)}>
                    {n.issue}
                  </button>
                )}
                {n.issue !== null && " · "}
                {metaLine(n)}
              </span>
            </li>
          ))}
        </ul>
      )}
      <AgentUpdates
        onAsk={(agent, update) => {
          setDrawer(false);
          onAskAgent(agent, update);
        }}
        onOpenIssue={openIssue}
      />
    </>
  );

  const tabs = (drawerMode: boolean) => (
    <div className="todo-tabs" role="group" aria-label={t("Home rail")}>
      <button aria-pressed={tab === "todo"} onClick={() => setTab("todo")}>
        {t("To do")} <span className={`todo-count ${count ? "on" : ""}`}>{overview.data ? count : "…"}</span>
      </button>
      <button aria-pressed={tab === "updates"} onClick={() => setTab("updates")}>
        {t("Updates")}
      </button>
      <span className="todo-tools">
        <Link href={overviewHref} className="lnk text-label" onClick={() => setDrawer(false)}>
          {t("Team overview")}
        </Link>
        {drawerMode ? (
          <button type="button" className="lnk text-label" aria-label={t("close the rail")} onClick={() => setDrawer(false)}>
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
            aria-label={t("to do — {count} waiting; expand the rail").replace("{count}", String(count))}
            onClick={onToggleCollapse}
          >
            <span className={`chip ${count ? "bg-warn/10 text-warn" : "bg-ok/15 text-ok"}`}>
              {overview.data ? count : "…"}
            </span>
            <span className="vert text-label text-ink-400">{t("To do")}</span>
            <span className="text-ink-500" aria-hidden>
              «
            </span>
          </button>
        ) : (
          <section className="card needsrail overflow-hidden" aria-label={t("to do")} data-collapsed="false">
            {tabs(false)}
            <div className="rail-scroll min-h-0">{panelBody}</div>
          </section>
        )}
      </div>

      {/* <1100px: a floating button opens the slide-over drawer. */}
      <button
        type="button"
        className="needbtn rail:hidden"
        ref={needBtn}
        onClick={() => setDrawer(true)}
        aria-label={t("to do — {count} waiting; open the list").replace("{count}", String(count))}
      >
        {t("To do")}
        <span className={`chip ${count ? "bg-warn/15 text-warn" : "bg-ok/15 text-ok"}`}>
          {overview.data ? count : "…"}
        </span>
      </button>
      {drawer && (
        <>
          <div className="needsscrim rail:hidden" onClick={() => setDrawer(false)} />
          <section
            className="needsdrawer rail:hidden"
            role="dialog"
            aria-label={t("to do")}
            aria-modal="true"
            tabIndex={-1}
            ref={drawerEl}
          >
            {tabs(true)}
            <div className="rail-scroll min-h-0 flex-1">{panelBody}</div>
          </section>
        </>
      )}

    </>
  );
}
