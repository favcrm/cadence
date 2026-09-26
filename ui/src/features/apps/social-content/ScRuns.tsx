/* ScRuns — workflow runs over records, each item pinned to its post. */
import Link from "../../../ui/Link";
import { api } from "./mock/api";
import { fmt } from "./fmt";
import { StatusDot, PageHead } from "./widgets";
import type { Run, StepState } from "./mock/data";

const STEP_SEQ = ["adapt", "visuals", "review", "schedule", "verify"];
const STEP_ICON: Record<StepState, string> = {
  done: "✓", running: "●", waiting: "", failed: "✕", mismatch: "≠", skipped: "–",
};

function Steps({ steps }: { steps: Run["items"][number]["steps"] }) {
  const seq = Object.keys(steps)[0] === "change" ? ["change"] : STEP_SEQ;
  return (
    <div className="sc-steps">
      {seq.map((s) => (
        <span key={s} className={`sc-step sc-${steps[s]}`}>
          <span className="sc-stic">{STEP_ICON[steps[s]] ?? ""}</span>
          {s}
        </span>
      ))}
    </div>
  );
}

export default function ScRuns({ postHref }: { postHref: (id: string) => string }) {
  const client = api.clients.current();
  const runs = api.runs
    .list()
    .filter((r) => r.client === client.id || r.kind === "revise")
    .sort((a, b) => (a.at < b.at ? 1 : -1));

  return (
    <main className="px-4 lg:px-8 pt-4 pb-9">
      <PageHead
        title="Runs"
        note="workflow runs over records — each item pinned to the post it produced"
      />
      <div className="flex flex-col gap-3">
        {runs.map((r) => (
          <div key={r.id} className="card p-3.5 flex flex-col gap-2.5">
            <div className="flex items-center gap-2.5 flex-wrap">
              <span className="num text-label font-semibold text-ink-100">{r.id}</span>
              <span className="chip bg-ink-800 text-ink-400">{r.kind}</span>
              <span className={`chip ${r.status === "done" ? "bg-ok/15 text-ok" : "bg-info/10 text-info"}`}>
                {r.status}
              </span>
              <span className="text-label text-ink-500">{r.label}</span>
              <span className="flex-1" />
              <span className="kicker">{fmt.ago(r.at)}</span>
            </div>
            <div className="flex flex-col gap-1.5">
              {r.items.map((it) => {
                const p = api.posts.get(it.post);
                return (
                  <div key={it.post} className="sc-ritem">
                    <StatusDot s={p ? p.status : "drafting"} />
                    <Link href={postHref(it.post)} className="lnk text-label text-left min-w-0 truncate">
                      {it.post} — {p && p.caption.text ? p.caption.text.slice(0, 60) : "(no caption yet)"}
                    </Link>
                    <Steps steps={it.steps} />
                  </div>
                );
              })}
            </div>
            <div className="sc-runlog">
              {r.log.slice(-6).map(([t, m], i) => (
                <div key={i} className="sc-l">
                  <span className="sc-lt num">{fmt.ago(t)}</span>
                  <span>{m}</span>
                </div>
              ))}
            </div>
          </div>
        ))}
      </div>
    </main>
  );
}
