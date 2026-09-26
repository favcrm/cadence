/* ScRuns — runs in plain language, for a non-technical reader: one row
 * per run with what it is, a state pill, progress and when. Ids, workflow
 * keys and step chains stay in the drawer, which shows a per-post
 * vertical stepper (Adapt → Visuals → Review → Schedule → Verify) plus
 * the activity log under "Show activity". */
import { useState } from "react";
import Link from "../../../ui/Link";
import { api } from "./mock/api";
import { fmt } from "./fmt";
import { PageHead } from "./widgets";
import ScDrawer from "./ScDrawer";
import type { Run, RunItem, StepState } from "./mock/data";

const STEP_LABEL: Record<string, string> = {
  adapt: "Adapt",
  visuals: "Visuals",
  review: "Review",
  schedule: "Schedule",
  verify: "Verify",
  change: "Change",
};
const STEP_SEQ = ["adapt", "visuals", "review", "schedule", "verify"];

const STEP_WORD: Record<StepState, string> = {
  done: "done",
  running: "working…",
  waiting: "waiting",
  failed: "failed",
  mismatch: "mismatch",
  skipped: "skipped",
};

type RunState = "running" | "needs" | "done";
export const STATE_PILL: Record<RunState, { label: string; cls: string }> = {
  running: { label: "running", cls: "bg-info/10 text-info" },
  needs: { label: "needs you", cls: "bg-fail/10 text-fail" },
  done: { label: "done", cls: "bg-ok/15 text-ok" },
};

const FILTERS: { id: RunState | "all"; label: string }[] = [
  { id: "all", label: "All" },
  { id: "running", label: "Running" },
  { id: "needs", label: "Needs you" },
  { id: "done", label: "Done" },
];

/** One plain sentence + the action to take, per failed step. */
const FAIL_HELP: Record<string, { what: string; act: string; to: "post" | "needs" }> = {
  review: {
    what: "The brand check didn't pass — the post went back to in review.",
    act: "open the post",
    to: "post",
  },
  verify: {
    what: "What went out doesn't match what you approved.",
    act: "resolve it in Needs you",
    to: "needs",
  },
  adapt: { what: "Drafting didn't finish for this post.", act: "open the post", to: "post" },
  visuals: { what: "No visual came back for this post.", act: "open the post", to: "post" },
  schedule: { what: "The post never made it into a digest.", act: "open the post", to: "post" },
};

export function runState(r: Run): RunState {
  const bad = r.items.some(
    (it) =>
      Object.values(it.steps).some((s) => s === "failed" || s === "mismatch") ||
      api.posts.get(it.post)?.status === "needs_you",
  );
  if (bad) return "needs";
  return r.status === "running" ? "running" : "done";
}

export function runWhat(r: Run): string {
  if (r.kind === "revise") {
    const m = r.label.match(/“([^”]+)”/);
    return `Revising caption for “${m ? m[1] : r.items[0]?.post ?? "a post"}”`;
  }
  if (r.kind === "scan-sources") return "Scanning sources for new posts";
  const n = r.items.length;
  return `Drafting ${n} post${n === 1 ? "" : "s"} from Library`;
}

/** Items settled = drafting steps all resolved (schedule/verify ride on
 * approvals and publishes, so they don't count against progress). */
function runProgress(r: Run): { ready: number; bad: number; total: number; pct: number } {
  let ready = 0,
    bad = 0;
  for (const it of r.items) {
    const seq = Object.keys(it.steps)[0] === "change" ? ["change"] : STEP_SEQ.slice(0, 3);
    const states = seq.map((s) => it.steps[s]);
    if (states.some((s) => s === "failed" || s === "mismatch")) bad++;
    else if (states.every((s) => s === "done" || s === "skipped")) ready++;
  }
  const total = r.items.length;
  return { ready, bad, total, pct: total ? Math.round(((ready + bad) / total) * 100) : 100 };
}

function progressText(r: Run): string {
  const { ready, bad, total } = runProgress(r);
  if (!total) return "—";
  const parts = [`${ready} of ${total} ready`];
  if (bad) parts.push(`${bad} need${bad === 1 ? "s" : ""} you`);
  return parts.join(" · ");
}

function Stepper({
  it,
  postHref,
  needsHref,
  onGone,
}: {
  it: RunItem;
  postHref: (id: string) => string;
  needsHref: string;
  onGone: () => void;
}) {
  const p = api.posts.get(it.post);
  const seq = Object.keys(it.steps)[0] === "change" ? ["change"] : STEP_SEQ;
  return (
    <div className="card p-3 flex flex-col gap-1">
      <div className="flex items-center gap-2 mb-1">
        <span className="slabel">post</span>
        <Link
          href={postHref(it.post)}
          className="lnk text-label truncate min-w-0"
          onClick={onGone}
        >
          {p?.caption.text ? p.caption.text.slice(0, 70) : it.post}
        </Link>
      </div>
      <div className="sc-vsteps">
        {seq.map((key) => {
          const st = it.steps[key] ?? "waiting";
          const fail = st === "failed" || st === "mismatch";
          const help = fail ? FAIL_HELP[key] : undefined;
          return (
            <div key={key} className={`sc-vstep sc-${st}`}>
              <span className="sc-stic">
                {st === "done" ? "✓" : st === "running" ? "●" : fail ? "✕" : st === "skipped" ? "–" : ""}
              </span>
              <span className="sc-vname">
                {STEP_LABEL[key] ?? key}
                {help && (
                  <span className="sc-vfail">
                    {help.what}{" "}
                    <Link
                      href={help.to === "needs" ? needsHref : postHref(it.post)}
                      className="lnk"
                      onClick={onGone}
                    >
                      {help.act} →
                    </Link>
                  </span>
                )}
              </span>
              <span className="sc-vword">{STEP_WORD[st]}</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function RunDrawer({
  r,
  postHref,
  needsHref,
  onClose,
}: {
  r: Run;
  postHref: (id: string) => string;
  needsHref: string;
  onClose: () => void;
}) {
  const st = runState(r);
  const pill = STATE_PILL[st];
  const { pct } = runProgress(r);
  return (
    <ScDrawer title={runWhat(r)} sub={`${r.id} · started ${fmt.ago(r.at)}`} onClose={onClose}>
      <div className="flex items-center gap-2.5">
        <span className={`chip ${pill.cls}`}>{pill.label}</span>
        <span className="text-label text-ink-400">{progressText(r)}</span>
        <span className="flex-1" />
      </div>
      <div className="sc-barwrap">
        <div className={`sc-bar sc-${st}`}>
          <i style={{ width: `${pct}%` }} />
        </div>
      </div>

      {r.items.length ? (
        r.items.map((it) => (
          <Stepper key={it.post} it={it} postHref={postHref} needsHref={needsHref} onGone={onClose} />
        ))
      ) : (
        <div className="card p-3 text-label text-ink-400">
          This run touched no posts — see the activity below.
        </div>
      )}

      <details className="sc-logbox">
        <summary className="lnk text-label">Show activity · {r.log.length}</summary>
        <div className="sc-runlog mt-2">
          {r.log.map(([t, m], i) => (
            <div key={i} className="sc-l">
              <span className="sc-lt num">{fmt.ago(t)}</span>
              <span>{m}</span>
            </div>
          ))}
        </div>
      </details>
    </ScDrawer>
  );
}

export default function ScRuns({
  postHref,
  needsHref,
}: {
  postHref: (id: string) => string;
  needsHref: string;
}) {
  const [filter, setFilter] = useState<RunState | "all">("all");
  const [openId, setOpenId] = useState<string | null>(null);
  const client = api.clients.current();
  const runs = api.runs
    .list()
    .filter((r) => r.client === client.id || r.kind === "revise")
    .sort((a, b) => (a.at < b.at ? 1 : -1));
  const shown = filter === "all" ? runs : runs.filter((r) => runState(r) === filter);
  const open = shown.find((r) => r.id === openId) ?? runs.find((r) => r.id === openId) ?? null;

  return (
    <main className="px-4 lg:px-8 pt-4 pb-9">
      <PageHead title="Runs" note="what the app is doing for you — one row per run" />
      <div className="flex items-center gap-1.5 mb-3 flex-wrap">
        {FILTERS.map((f) => (
          <button
            key={f.id}
            className={`chip !py-1 ${filter === f.id ? "bg-accent/15 text-accent" : "bg-ink-800 text-ink-400 hover:text-ink-200"}`}
            aria-pressed={filter === f.id}
            onClick={() => setFilter(f.id)}
          >
            {f.label}
            {f.id !== "all" && (
              <span className="num">{runs.filter((r) => runState(r) === f.id).length}</span>
            )}
          </button>
        ))}
      </div>

      <div className="flex flex-col gap-2 max-w-3xl">
        {shown.map((r) => {
          const st = runState(r);
          const pill = STATE_PILL[st];
          const { pct } = runProgress(r);
          return (
            <button
              key={r.id}
              className="card tcard sc-runrow text-left"
              onClick={() => setOpenId(r.id)}
            >
              <span className={`chip ${pill.cls} sc-rpill`}>{pill.label}</span>
              <span className="sc-rwhat">{runWhat(r)}</span>
              <span className="sc-rprog">
                <span className="sc-rptext">{progressText(r)}</span>
                {r.items.length > 0 && (
                  <span className={`sc-bar sc-${st}`}>
                    <i style={{ width: `${pct}%` }} />
                  </span>
                )}
              </span>
              <span className="sc-rwhen">{fmt.ago(r.at)}</span>
              <span className="sc-rchev">›</span>
            </button>
          );
        })}
        {!shown.length && (
          <div className="card p-6 text-center">
            <div className="text-label text-ink-500">No runs here — try another filter.</div>
          </div>
        )}
      </div>

      {open && (
        <RunDrawer
          key={open.id}
          r={open}
          postHref={postHref}
          needsHref={needsHref}
          onClose={() => setOpenId(null)}
        />
      )}
    </main>
  );
}
