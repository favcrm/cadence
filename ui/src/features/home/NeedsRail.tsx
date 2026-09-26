import { useWriteBlock } from "../auth/WriteGate";
import { useState } from "react";
import { api, ApiError } from "../../lib/api";
import type { ResourceState } from "../../lib/cache";
import { resources } from "../../lib/resources";
import type { Overview } from "../../lib/types";
import {
  ageLabel,
  homeNeeds,
  needGroups,
  UNFENCE_CHOICES,
  type HomeNeed,
  type NeedGroupKey,
  type UnfenceChoice,
} from "./needs";
import PlanCard from "./PlanCard";
import Link from "../../ui/Link";

/** One row of a rail's inline menu (the `…` overflow and the Unfence
 *  reconcile choice share it). */
const MENU_ITEM =
  "needitem w-full text-left px-2.5 py-1.5 text-label text-ink-200 hover:bg-ink-800 rounded disabled:opacity-40 disabled:hover:bg-transparent";

const KIND_CHIP: Record<string, string> = {
  plan: "bg-warn/10 text-warn",
  question: "bg-info/10 text-info",
  approval: "bg-warn/10 text-warn",
  merge: "bg-ok/15 text-ok",
};

const SNOOZE_24H = 86_400;
const SNOOZE_7D = 604_800;

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

/** The merge decision (CAD-431): what passed, then one Merge button. */
function MergeForm({
  need,
  readOnly,
  onDone,
}: {
  need: HomeNeed & { action: { type: "merge" } };
  readOnly: boolean;
  onDone: (text: string) => void;
}) {
  const { issue, pr, sha, reviewer, verdict } = need.action;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const merge = () => {
    setBusy(true);
    setError(null);
    api
      .mergeDelivery(issue)
      .then((out) => {
        void resources.overview.invalidate();
        onDone(`merge ${String((out as { state?: unknown }).state ?? "sent")}`);
      })
      .catch((e: ApiError) => setError(e.message ?? String(e)))
      .finally(() => setBusy(false));
  };
  return (
    <div className="mt-2 space-y-2">
      <p className="text-micro text-ink-500 break-words">
        {pr ?? issue}
        {sha ? ` · head ${sha.slice(0, 12)}` : ""}
        {reviewer ? ` · PASS by ${reviewer}` : ""}
      </p>
      {verdict && <p className="text-label text-ink-300 break-words">{verdict}</p>}
      {readOnly ? (
        <p className="text-micro text-ink-500">Board is read-only — merge with `cadence delivery merge {issue}`.</p>
      ) : (
        <button
          disabled={busy}
          onClick={merge}
          className="h-8 px-3 rounded bg-accent text-on-accent text-label font-medium disabled:opacity-40"
        >
          {busy ? "Merging…" : "Merge"}
        </button>
      )}
      {error && (
        <p className="text-micro text-fail break-words" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

/**
 * The `…` overflow (CAD-574): Open lands on the row's own link or its
 * issue page; Snooze and Dismiss are the operator's `needs_dismiss`
 * routes; Copy command keeps the row's fallback command one tap away —
 * demoted off the row because Ask master is the action now.
 */
function NeedMenu({
  need,
  block,
  busy,
  onOpenIssue,
  onDecide,
  onCopied,
  onClose,
}: {
  need: HomeNeed;
  block: string | null;
  busy: boolean;
  onOpenIssue: (id: string) => void;
  onDecide: (verb: "snooze" | "dismiss", secs?: number) => void;
  onCopied: () => void;
  onClose: () => void;
}) {
  const item = MENU_ITEM;
  return (
    <>
      <button
        type="button"
        aria-hidden
        tabIndex={-1}
        className="fixed inset-0 z-20 cursor-default"
        onClick={onClose}
      />
      <div className="needmenu" role="menu" aria-label={`actions for ${need.title}`}>
        {need.link && (
          <a
            className={`${item} block`}
            href={need.link}
            target="_blank"
            rel="noreferrer"
            role="menuitem"
          >
            Open
          </a>
        )}
        {!need.link && need.issue && (
          <button className={item} role="menuitem" onClick={() => onOpenIssue(need.issue!)}>
            Open
          </button>
        )}
        <button
          className={item}
          role="menuitem"
          disabled={!!block || busy}
          title={block ?? undefined}
          onClick={() => onDecide("snooze", SNOOZE_24H)}
        >
          Snooze 24h
        </button>
        <button
          className={item}
          role="menuitem"
          disabled={!!block || busy}
          title={block ?? undefined}
          onClick={() => onDecide("snooze", SNOOZE_7D)}
        >
          Snooze 7d
        </button>
        <button
          className={`${item} text-fail`}
          role="menuitem"
          disabled={!!block || busy}
          title={block ?? undefined}
          onClick={() => onDecide("dismiss")}
        >
          Dismiss
        </button>
        <button
          className={item}
          role="menuitem"
          onClick={() => {
            copy(need.command).then(onCopied, () => undefined);
            onClose();
          }}
        >
          Copy command
        </button>
      </div>
    </>
  );
}

function NeedItem({
  need,
  readOnly,
  onOpenIssue,
  onAsk,
  onHide,
}: {
  need: HomeNeed;
  readOnly: boolean;
  onOpenIssue: (id: string) => void;
  onAsk: (need: HomeNeed) => void;
  onHide: (key: string) => void;
}) {
  const block = useWriteBlock(readOnly);
  const [open, setOpen] = useState(false);
  const [menu, setMenu] = useState(false);
  const [pick, setPick] = useState(false);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const action = need.action;

  const refresh = () => {
    void resources.overview.invalidate();
    void resources.agents.invalidate();
  };
  const run = (work: () => Promise<unknown>, what: string, hide: boolean) => {
    setBusy(true);
    setError(null);
    work()
      .then(() => {
        setDone(what);
        setMenu(false);
        if (hide) onHide(need.key);
        refresh();
      })
      .catch((e: ApiError) => setError(e.message ?? String(e)))
      .finally(() => setBusy(false));
  };
  const decide = (verb: "snooze" | "dismiss", secs?: number) => {
    const id = need.subject ?? { kind: "row", id: need.key };
    run(
      () => api.needDecide(verb, id.kind, id.id, secs),
      verb === "dismiss" ? "dismissed" : secs === SNOOZE_7D ? "snoozed 7d" : "snoozed 24h",
      true,
    );
  };
  const agentAct = (verb: "resume" | "unfence", status?: UnfenceChoice["status"]) => {
    if (!need.agent) return;
    run(() => api.agentAct(need.agent!, verb, status), verb === "resume" ? "resumed" : `unfenced · ${status}`, true);
  };

  const expandLabel =
    action.type === "plan" ? "Review plan" : action.type === "answer" ? "Answer" : "Review merge";
  return (
    <li className="needrow px-3 py-2 min-w-0" data-need={need.kind}>
      <div className="flex items-center gap-2 min-w-0">
        <span className={`chip shrink-0 ${KIND_CHIP[need.kind] ?? "bg-ink-800 text-ink-300"}`}>{need.label}</span>
        <div className="min-w-0 flex-1 truncate text-secondary text-ink-200" title={need.title}>
          {need.title}
        </div>
        <span className="num shrink-0 text-micro text-ink-500" title={`waiting ${ageLabel(need.age)}`}>
          {ageLabel(need.age)}
        </span>
      </div>
      <div className="text-micro text-ink-500 mt-0.5 truncate">
        {need.owner}
        {need.escalatedBy ? ` · escalated by ${need.escalatedBy}` : ""}
      </div>
      {done ? (
        <p className="text-micro text-ok mt-1" data-need-done>
          {done}
        </p>
      ) : (
        <div className="mt-1 flex items-center gap-2 flex-wrap min-w-0">
          <button
            className="needask shrink-0"
            disabled={!!block}
            title={block ?? "Prefill the composer — nothing sends until you press Enter"}
            onClick={() => onAsk(need)}
          >
            Ask master
          </button>
          {(action.type === "plan" || action.type === "answer" || action.type === "merge") && (
            <button
              className="lnk text-label shrink-0"
              aria-expanded={open}
              onClick={() => setOpen((o) => !o)}
            >
              {expandLabel} {open ? "▴" : "▾"}
            </button>
          )}
          {need.kind === "fenced" && need.agent && (
            <button
              className="lnk text-label shrink-0"
              disabled={!!block || busy}
              aria-expanded={pick}
              title={block ?? `cadence agent unfence ${need.agent} — say how the fenced turns settled`}
              onClick={() => {
                setMenu(false);
                setPick(true);
              }}
            >
              Unfence
            </button>
          )}
          {need.kind === "stopped" && need.agent && (
            <button
              className="lnk text-label shrink-0"
              disabled={!!block || busy}
              title={block ?? `cadence agent resume ${need.agent}`}
              onClick={() => agentAct("resume")}
            >
              Resume
            </button>
          )}
          {busy && <span className="text-micro text-ink-500">working…</span>}
          {note && <span className="text-micro text-ok">{note}</span>}
          <button
            className="needmore ml-auto shrink-0"
            aria-label={`more actions for ${need.title}`}
            aria-expanded={menu}
            onClick={() => setMenu((m) => !m)}
          >
            ⋯
          </button>
        </div>
      )}
      {pick && !done && (
        <>
          <button
            type="button"
            aria-hidden
            tabIndex={-1}
            className="fixed inset-0 z-20 cursor-default"
            onClick={() => setPick(false)}
          />
          <div className="needmenu" role="menu" aria-label={`reconcile ${need.agent}'s fenced turns as`}>
            <p className="px-2.5 pt-1.5 pb-1 text-micro text-ink-500">
              How did {need.agent}'s fenced turns end? It resumes after.
            </p>
            {UNFENCE_CHOICES.map((c) => (
              <button
                key={c.status}
                className={MENU_ITEM}
                role="menuitem"
                disabled={busy}
                onClick={() => {
                  setPick(false);
                  agentAct("unfence", c.status);
                }}
              >
                <span className="block">{c.status}</span>
                <span className="block text-micro text-ink-500">{c.blurb}</span>
              </button>
            ))}
          </div>
        </>
      )}
      {menu && !done && (
        <NeedMenu
          need={need}
          block={block}
          busy={busy}
          onOpenIssue={(id) => {
            setMenu(false);
            onOpenIssue(id);
          }}
          onDecide={decide}
          onCopied={() => {
            setNote("copied");
            setTimeout(() => setNote(null), 1500);
          }}
          onClose={() => setMenu(false)}
        />
      )}
      {error && (
        <p className="text-micro text-fail mt-1 break-words" role="alert">
          {error}
        </p>
      )}
      {open && action.type === "plan" && (
        <div className="mt-2">
          <PlanCard epic={action.epic} readOnly={readOnly} onOpenIssue={onOpenIssue} />
        </div>
      )}
      {open && action.type === "merge" && !done && (
        <MergeForm
          need={need as HomeNeed & { action: { type: "merge" } }}
          readOnly={readOnly}
          onDone={(t) => {
            setDone(t);
            setOpen(false);
          }}
        />
      )}
      {open && action.type === "answer" && !done && (
        <AnswerForm
          need={need as HomeNeed & { action: { type: "answer" } }}
          readOnly={readOnly}
          onDone={(t) => {
            setDone(`answered: ${t}`);
            setOpen(false);
          }}
        />
      )}
    </li>
  );
}

/** One collapsible group of needs — `Decisions` opens by default. */
function NeedGroup({
  groupKey,
  label,
  needs,
  open,
  onToggle,
  readOnly,
  onOpenIssue,
  onAsk,
  onHide,
}: {
  groupKey: NeedGroupKey | "old";
  label: string;
  needs: HomeNeed[];
  open: boolean;
  onToggle: () => void;
  readOnly: boolean;
  onOpenIssue: (id: string) => void;
  onAsk: (need: HomeNeed) => void;
  onHide: (key: string) => void;
}) {
  return (
    <section data-need-group={groupKey}>
      <button
        type="button"
        className="needgroup w-full"
        aria-expanded={open}
        onClick={onToggle}
      >
        <span className="steps-caret num" aria-hidden>
          ›
        </span>
        <span className="flex-1 text-left truncate">{label}</span>
        <span className="chip bg-ink-800 text-ink-400 shrink-0">{needs.length}</span>
      </button>
      {open && (
        <ul className="divide-y divide-ink-700">
          {needs.map((n) => (
            <NeedItem
              key={n.key}
              need={n}
              readOnly={readOnly}
              onOpenIssue={onOpenIssue}
              onAsk={onAsk}
              onHide={onHide}
            />
          ))}
        </ul>
      )}
    </section>
  );
}

/**
 * "Needs you" (CAD-574): its own column, sticky under the header with
 * its own scroll — the chat beside it never shares the scrollbar. It
 * collapses to a slim rail with a count badge (persisted per viewer),
 * and under ~1100px it is a slide-over drawer opened from the floating
 * button. Items group as Decisions / PRs / Blocked→ready / Inboxes /
 * Other — Decisions open, the rest folded; rows past 14 days fold under
 * `Old (n)`. The action is Ask master (a prefilled composer draft —
 * never auto-sent); Open, Snooze, Dismiss, Copy command live in the `…`
 * menu, Resume/Unfence appear on fenced/stopped agent rows.
 */
export default function NeedsRail({
  overview,
  readOnly,
  onOpenIssue,
  overviewHref,
  onAsk,
  collapsed,
  onToggleCollapse,
}: {
  overview: ResourceState<Overview>;
  readOnly: boolean;
  onOpenIssue: (id: string) => void;
  overviewHref: string;
  onAsk: (need: HomeNeed) => void;
  collapsed: boolean;
  onToggleCollapse: () => void;
}) {
  const needs = homeNeeds(overview.data?.needs_me);
  const { groups, old } = needGroups(needs);
  const [openGroups, setOpenGroups] = useState<ReadonlySet<NeedGroupKey>>(
    () => new Set<NeedGroupKey>(["decisions"]),
  );
  const [oldOpen, setOldOpen] = useState(false);
  const [drawer, setDrawer] = useState(false);
  const [hidden, setHidden] = useState<ReadonlySet<string>>(() => new Set());
  const count = needs.filter((n) => !hidden.has(n.key)).length;

  const toggleGroup = (key: NeedGroupKey) =>
    setOpenGroups((s) => {
      const next = new Set(s);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  const hide = (key: string) => setHidden((s) => new Set(s).add(key));
  const ask = (need: HomeNeed) => {
    setDrawer(false);
    onAsk(need);
  };

  const body = (
    <>
      {!overview.data && overview.status === "failed" && (
        <p className="px-3 py-2.5 text-label text-fail break-words">Overview unavailable — {overview.error}</p>
      )}
      {!overview.data && overview.status !== "failed" && (
        <p className="px-3 py-2.5 text-label text-ink-500">Reading what needs you…</p>
      )}
      {overview.data && needs.length === 0 && (
        <p className="px-3 py-2.5 text-label text-ink-500">Nothing needs a decision. The team is working.</p>
      )}
      {groups.map((g) => (
        <NeedGroup
          key={g.key}
          groupKey={g.key}
          label={g.label}
          needs={g.needs.filter((n) => !hidden.has(n.key))}
          open={openGroups.has(g.key)}
          onToggle={() => toggleGroup(g.key)}
          readOnly={readOnly}
          onOpenIssue={onOpenIssue}
          onAsk={ask}
          onHide={hide}
        />
      ))}
      {old.length > 0 && (
        <NeedGroup
          groupKey="old"
          label={`Old (${old.length})`}
          needs={old.filter((n) => !hidden.has(n.key))}
          open={oldOpen}
          onToggle={() => setOldOpen((o) => !o)}
          readOnly={readOnly}
          onOpenIssue={onOpenIssue}
          onAsk={ask}
          onHide={hide}
        />
      )}
    </>
  );

  const header = (drawerMode: boolean) => (
    <header className="px-3 py-2.5 flex items-center gap-2 border-b border-ink-700 shrink-0">
      <h2 className="text-secondary font-semibold text-ink-100">Needs you</h2>
      <span className={`chip ${count ? "bg-warn/10 text-warn" : "bg-ok/15 text-ok"}`}>
        {overview.data ? count || "clear" : "…"}
      </span>
      <Link href={overviewHref} className="lnk text-label ml-auto" onClick={() => setDrawer(false)}>
        Team overview
      </Link>
      {drawerMode ? (
        <button
          type="button"
          className="lnk text-label shrink-0"
          aria-label="close needs panel"
          onClick={() => setDrawer(false)}
        >
          ✕
        </button>
      ) : (
        <button
          type="button"
          className="lnk text-label shrink-0"
          aria-label={collapsed ? "expand needs rail" : "collapse needs rail"}
          onClick={onToggleCollapse}
        >
          {collapsed ? "»" : "«"}
        </button>
      )}
    </header>
  );

  return (
    <>
      {/* ≥1100px: the detached column — sticky in the grid, scrolling on
          its own; collapsed it is the slim rail with the badge. */}
      <div className="hidden rail:block h-full min-h-0">
        {collapsed ? (
          <button
            type="button"
            className="slimrail card"
            aria-label={`needs you — ${count} waiting; expand the rail`}
            onClick={onToggleCollapse}
          >
            <span className={`chip ${count ? "bg-warn/10 text-warn" : "bg-ok/15 text-ok"}`}>
              {overview.data ? count : "…"}
            </span>
            <span className="vert text-label text-ink-400">Needs you</span>
            <span className="text-ink-500" aria-hidden>
              «
            </span>
          </button>
        ) : (
          <section className="card needsrail overflow-hidden" aria-label="needs you" data-collapsed="false">
            {header(false)}
            <div className="rail-scroll min-h-0">{body}</div>
          </section>
        )}
      </div>

      {/* <1100px: a floating button opens the slide-over drawer. */}
      <button
        type="button"
        className="needbtn rail:hidden"
        onClick={() => setDrawer(true)}
        aria-label={`needs you — ${count} waiting; open the panel`}
      >
        Needs you
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
            aria-label="needs you"
            aria-modal="true"
          >
            {header(true)}
            <div className="rail-scroll min-h-0 flex-1">{body}</div>
          </section>
        </>
      )}
    </>
  );
}
