import { useEffect, useRef, useState } from "react";
import { api, type ApiError } from "../../lib/api";
import type { ResourceState } from "../../lib/cache";
import { fmtTime } from "../../lib/fmt";
import { cache, resources } from "../../lib/resources";
import type { IssueCard, IssueHistoryEntry, WorkBlock } from "../../lib/types";
import { useQuery } from "../../lib/useResource";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import { IconChevron } from "../../ui/icons";
import { HealthBadge, HealthReasons, ProgressBar } from "./HealthBadge";
import {
  childrenOf,
  epicsOf,
  healthView,
  noMoveReason,
  noteBytes,
  NOTE_MAX_BYTES,
  progressView,
  stageEvents,
  stageLabel,
  visibleMoves,
  type Viewer,
} from "./work";

/** An epic's tracker history — the stage moves are read out of it. */
const epicHistory = cache.family<string, IssueHistoryEntry[]>("epic-history", (id) =>
  api.history(id, 200).then((r) => r.history),
);

const STATUS_TEXT: Record<string, string> = {
  done: "text-ok",
  doing: "text-accent",
  review: "text-info",
  dropped: "text-ink-500 line-through",
};

/**
 * Projects → a project → Epics (CAD-432): each epic with its stage,
 * size-weighted progress, health (with the reason and next action) and
 * milestone. Rows come from the `/api/issues` cards' `work` block, so the
 * tracker stream keeps them live. Selecting an epic opens its children,
 * its stage history (who moved each stage, when) and — for the operator
 * only — the legal stage moves.
 */
export default function Epics({
  project,
  issues,
  viewer,
  onOpenIssue,
  onRetry,
}: {
  project: string;
  issues: ResourceState<IssueCard[]>;
  viewer: Viewer;
  onOpenIssue: (id: string) => void;
  onRetry: () => void;
}) {
  const [showCompleted, setShowCompleted] = useState(false);
  const [open, setOpen] = useState<string | null>(null);
  const cards = issues.data ?? [];
  const allEpics = epicsOf(cards, project);
  const completed = allEpics.filter((e) => e.status === "done" || e.status === "dropped");
  const epics = allEpics.filter((e) => showCompleted || !completed.includes(e)).sort((a, b) => Number(b.work.health?.state === "at_risk" || b.work.health?.state === "stalled") - Number(a.work.health?.state === "at_risk" || a.work.health?.state === "stalled"));
  const notes = [
    ...new Set(epics.flatMap((e) => [e.work.config_unapproved, e.work.config_error].filter(Boolean) as string[])),
  ];
  const tally = { on_track: 0, at_risk: 0, stalled: 0 } as Record<string, number>;
  for (const e of epics) {
    const s = e.work.health?.state;
    if (s && s in tally) tally[s] += 1;
  }

  return (
    <main className="px-4 lg:px-8 pt-4 pb-9 min-w-0" aria-label="epics">
      <div className="flex flex-wrap items-center gap-2 mb-3">
        <h1 className="text-section font-semibold text-ink-100">Epics</h1>
        {completed.length > 0 && <button className="lnk text-label ml-auto" aria-pressed={showCompleted} onClick={() => setShowCompleted(!showCompleted)}>{showCompleted ? "Hide" : "Show"} completed ({completed.length})</button>}
        <StaleChip state={issues} />
        {epics.length > 0 && (
          <span className="text-label text-ink-500 num">
            {tally.on_track} on track · {tally.at_risk} at risk · {tally.stalled} stalled
          </span>
        )}
      </div>
      <p className="text-label text-ink-500 mb-5">Capability outcomes: their delivery stage, owner, and current work. An epic can contribute to several milestones.</p>
      <ResourceGate state={issues} loading="loading epics…" failed="could not load epics" onRetry={onRetry} />
      {notes.map((n) => (
        <p key={n} className="card mb-3 px-3.5 py-2.5 text-label text-warn break-words" role="note">
          {n}
        </p>
      ))}
      {issues.data && epics.length === 0 && (
        <div className="card px-4 py-5 text-secondary text-ink-400">
          No active epics in {project}. Group related tasks into an epic when they deliver one capability or outcome.
        </div>
      )}
      <ul className="space-y-2.5">
        {epics.map((e) => (
          <EpicRow
            key={e.id}
            epic={e}
            cards={cards}
            open={open === e.id}
            viewer={viewer}
            onToggle={() => setOpen((cur) => (cur === e.id ? null : e.id))}
            onOpenIssue={onOpenIssue}
          />
        ))}
      </ul>
    </main>
  );
}

function EpicRow({
  epic,
  cards,
  open,
  viewer,
  onToggle,
  onOpenIssue,
}: {
  epic: IssueCard & { work: WorkBlock };
  cards: IssueCard[];
  open: boolean;
  viewer: Viewer;
  onToggle: () => void;
  onOpenIssue: (id: string) => void;
}) {
  const w = epic.work;
  const health = healthView(w.health, w.stage?.id);
  const progress = progressView(w.progress);
  const kids = childrenOf(cards, epic.id);
  const checkpoints = [...new Set(kids.map((k) => k.work?.milestone ?? w.milestone).filter(Boolean))];
  if (!checkpoints.length && w.milestone) checkpoints.push(w.milestone);
  const current = kids.filter((k) => !["done", "dropped"].includes(k.status))
    .sort((a, b) => Number(b.blocked || b.status === "review") - Number(a.blocked || a.status === "review"));
  return (
    <li className="card min-w-0" data-epic={epic.id}>
      <div className="px-3.5 py-3 min-w-0">
        <button
          type="button"
          onClick={onToggle}
          aria-expanded={open}
          aria-label={`${open ? "close" : "open"} ${epic.id}`}
          className="w-full text-left flex items-start gap-2 min-w-0 rounded hover:text-accent"
        >
          <span className="num text-label text-accent shrink-0 pt-px">{epic.id}</span>
          <span className="text-cardtitle font-medium text-ink-100 min-w-0 break-words flex-1">{epic.title}</span>
          <IconChevron
            className={`shrink-0 mt-1.5 text-ink-500 transition-transform ${open ? "rotate-180" : ""}`}
          />
        </button>
        <div className="flex flex-wrap items-center gap-1.5 mt-2">
          <span className="chip bg-accent/10 text-accent" title={w.stage?.exit ? `exit: ${w.stage.exit}` : undefined}>
            {stageLabel(w.stage)}
          </span>
          <HealthBadge view={health} />
          <span className="text-micro text-ink-400">Owner: {epic.owner ?? "Unassigned"}</span>
          {health.timing && <span className="text-micro text-ink-500">{health.timing}</span>}
        </div>
        {w.stage?.exit && <p className="text-label text-ink-400 mt-2 max-w-[85ch]">Next stage requires: {w.stage.exit}</p>}
        {current[0] && <button className="text-left text-label text-ink-400 hover:text-accent mt-3 min-h-8" onClick={() => onOpenIssue(current[0].id)}><span className="text-ink-500">{current[0].blocked ? "Blocked" : current[0].status === "review" ? "In review" : "Current work"} · </span>{current[0].title} <span className="num text-micro">({current[0].id})</span></button>}
        {!!checkpoints.length && <p className="text-micro text-ink-500 mt-2">Contributes to {checkpoints.join(", ")}</p>}
        <div className="mt-2.5">
          <ProgressBar view={progress} tone={health.tone} />
        </div>
        {health.reasons.length > 0 && (
          <div className="mt-2.5">
            <HealthReasons view={health} />
          </div>
        )}
      </div>
      {open && <EpicDetail epic={epic} cards={cards} viewer={viewer} onOpenIssue={onOpenIssue} />}
    </li>
  );
}

function EpicDetail({
  epic,
  cards,
  viewer,
  onOpenIssue,
}: {
  epic: IssueCard & { work: WorkBlock };
  cards: IssueCard[];
  viewer: Viewer;
  onOpenIssue: (id: string) => void;
}) {
  const kids = childrenOf(cards, epic.id);
  const history = useQuery(epicHistory(epic.id));
  // A new revision of the epic (a move from anywhere) re-reads its history.
  const seenRev = useRef(epic.rev);
  useEffect(() => {
    if (seenRev.current === epic.rev) return;
    seenRev.current = epic.rev;
    void epicHistory(epic.id).invalidate();
  }, [epic.id, epic.rev]);
  const events = stageEvents(history.data ?? []);
  const stage = epic.work.stage;
  const moves = visibleMoves(stage, viewer);
  const why = noMoveReason(stage, viewer);
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);
  const bytes = noteBytes(note);
  const noteTooLong = bytes > NOTE_MAX_BYTES;

  const move = (to: string) => {
    setBusy(to);
    setResult(null);
    api
      .moveStage(epic.id, to, note)
      .then((out) => {
        setNote("");
        setResult({ ok: true, text: `Moved ${out.from} → ${out.to}` });
        void resources.issues.invalidate();
        void epicHistory(epic.id).invalidate();
        cache.invalidate("milestones");
      })
      .catch((e: ApiError) => setResult({ ok: false, text: e.message ?? String(e) }))
      .finally(() => setBusy(null));
  };

  return (
    <div className="border-t border-ink-700 px-3.5 py-3 grid gap-4 md:grid-cols-2 min-w-0">
      <section className="min-w-0" aria-label={`${epic.id} children`}>
        <div className="slabel mb-1.5">Tasks · {kids.length}</div>
        {kids.length === 0 ? (
          <p className="text-label text-ink-500">No children yet.</p>
        ) : (
          <ul className="divide-y divide-ink-700">
            {kids.map((k) => (
              <li key={k.id} className="py-1.5 flex items-start gap-2 min-w-0">
                <button className="lnk num text-label shrink-0" onClick={() => onOpenIssue(k.id)}>
                  {k.id}
                </button>
                <span className="text-secondary text-ink-200 min-w-0 flex-1 break-words">{k.title}</span>
                <span className="chip bg-ink-800 text-ink-300 shrink-0" title="size (S=1, M=3, L=8)">
                  {k.work?.size ?? "M?"}
                </span>
                <span
                  className={`text-micro num shrink-0 ${k.blocked ? "text-warn" : (STATUS_TEXT[k.status] ?? "text-ink-400")}`}
                >
                  {k.blocked ? "blocked" : k.status}
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="min-w-0 space-y-3" aria-label={`${epic.id} stage`}>
        <div>
          <div className="slabel mb-1.5">stage</div>
          {stage?.exit && (
            <p className="text-label text-ink-400 break-words">
              To leave <span className="text-ink-200">{stage.id}</span>: {stage.exit}
            </p>
          )}
          {moves.length > 0 && (
            <div className="mt-2 space-y-2">
              <input
                value={note}
                onChange={(e) => setNote(e.target.value)}
                className="field w-full"
                placeholder="Note for the move (optional)"
                aria-label="stage move note"
                aria-invalid={noteTooLong}
              />
              <p className={`text-micro num ${noteTooLong ? "text-fail" : "text-ink-500"}`} aria-live="polite">
                {bytes}/{NOTE_MAX_BYTES} bytes{noteTooLong ? " — shorten the note to move" : ""}
              </p>
              <div className="flex flex-wrap gap-2">
                {moves.map((m) => (
                  <button
                    key={m.to}
                    data-move={m.to}
                    disabled={busy !== null || noteTooLong}
                    onClick={() => move(m.to)}
                    className={
                      m.forward
                        ? "h-8 px-3 rounded bg-accent text-on-accent text-label font-medium disabled:opacity-40"
                        : "h-8 px-3 rounded border border-ink-600 text-label text-ink-300 hover:border-edge-hover disabled:opacity-40"
                    }
                    title={m.needs_operator ? "operator decision" : undefined}
                  >
                    {busy === m.to ? "Moving…" : m.forward ? `Move to ${m.to}` : `Back to ${m.to}`}
                    {m.needs_operator && <span className="ml-1.5 opacity-80">· operator</span>}
                  </button>
                ))}
              </div>
            </div>
          )}
          {moves.length === 0 && why && <p className="text-label text-ink-500 mt-1 break-words">{why}</p>}
          {result && (
            <p className={`text-label mt-1.5 break-words ${result.ok ? "text-ok" : "text-fail"}`} role="status">
              {result.text}
            </p>
          )}
        </div>
        <div>
          <div className="slabel mb-1.5">stage history</div>
          {history.status === "loading" && <p className="text-label text-ink-500">Loading…</p>}
          {history.status === "failed" && (
            <p className="text-label text-fail break-words">Could not read the history — {history.error}</p>
          )}
          {history.data && events.length === 0 && (
            <p className="text-label text-ink-500">
              Never moved — {stage ? `reads as ${stage.id} (${stage.source})` : "no stage"}.
            </p>
          )}
          {events.length > 0 && (
            <ol className="space-y-1.5">
              {events.map((ev, i) => (
                <li key={i} className="text-label min-w-0">
                  <span className="text-ink-200">{ev.what}</span>
                  <span className="text-ink-500">
                    {" "}
                    · {ev.by} · <span className="num">{fmtTime(ev.at)}</span>
                  </span>
                  {ev.note && <span className="block text-ink-400 break-words">{ev.note}</span>}
                </li>
              ))}
            </ol>
          )}
        </div>
      </section>
    </div>
  );
}
