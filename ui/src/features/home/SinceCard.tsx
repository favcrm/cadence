import { useEffect, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { awayLabel, awaySince, readLastSeen, summaryView, writeLastSeen, type SummaryView } from "./sinceLeft";

type Load =
  | { kind: "loading" }
  | { kind: "ok"; view: SummaryView }
  | { kind: "unavailable" }
  | { kind: "error"; message: string };

const nowSecs = () => Math.floor(Date.now() / 1000);

/**
 * "Since you left" (CAD-328): on a return after at least an hour, a
 * compact card with the daemon's `master_summary` since this browser
 * last had Home open. The last-seen time is kept per browser and
 * refreshed while Home is visible.
 */
export default function SinceCard({ onOpenIssue }: { onOpenIssue: (id: string) => void }) {
  // Read once, before this visit overwrites it.
  const [since] = useState(() => awaySince(readLastSeen(), nowSecs()));
  const [load, setLoad] = useState<Load>({ kind: "loading" });
  const [dismissed, setDismissed] = useState(false);

  useEffect(() => {
    const mark = () => {
      if (document.visibilityState === "visible") writeLastSeen(nowSecs());
    };
    mark();
    const timer = setInterval(mark, 60_000);
    const onHide = () => writeLastSeen(nowSecs());
    document.addEventListener("visibilitychange", mark);
    addEventListener("pagehide", onHide);
    return () => {
      clearInterval(timer);
      onHide();
      document.removeEventListener("visibilitychange", mark);
      removeEventListener("pagehide", onHide);
    };
  }, []);

  useEffect(() => {
    if (since === null) return;
    let live = true;
    api
      .masterSummary(since)
      .then((s) => live && setLoad({ kind: "ok", view: summaryView(s) }))
      .catch((e: ApiError) => {
        if (!live) return;
        if (e.status === 501 || e.status === 404) setLoad({ kind: "unavailable" });
        else setLoad({ kind: "error", message: e.message ?? String(e) });
      });
    return () => {
      live = false;
    };
  }, [since]);

  if (since === null || dismissed) return null;
  const away = awayLabel(nowSecs() - since);
  return (
    <section className="card px-3.5 py-2.5" aria-label="since you left" data-since-card>
      <div className="flex items-center gap-2">
        <h2 className="text-secondary font-semibold text-ink-100">Since you left</h2>
        <span className="text-label text-ink-500 num">· {away}</span>
        <button
          className="ml-auto text-label text-ink-500 hover:text-ink-200"
          onClick={() => setDismissed(true)}
          aria-label="dismiss since you left"
        >
          ✕
        </button>
      </div>
      {load.kind === "loading" && <p className="text-label text-ink-500 mt-1">Reading what happened…</p>}
      {load.kind === "unavailable" && (
        <p className="text-label text-ink-500 mt-1">
          The summary needs a daemon with the master (`master_summary`) — not available here.
        </p>
      )}
      {load.kind === "error" && (
        <p className="text-label text-fail mt-1 break-words">Summary unavailable — {load.message}</p>
      )}
      {load.kind === "ok" && load.view.quiet && (
        <p className="text-label text-ink-400 mt-1">Nothing moved — no plans, tickets, reports or questions.</p>
      )}
      {load.kind === "ok" && !load.view.quiet && (
        <dl className="mt-1.5 grid grid-cols-1 sm:grid-cols-2 gap-x-4 gap-y-1.5">
          {load.view.sections.map((sec) => (
            <div key={sec.label} className="min-w-0">
              <dt className="slabel">
                {sec.label} <span className="num">{sec.rows === null ? "?" : sec.rows.length}</span>
              </dt>
              <dd className="text-label text-ink-300 min-w-0">
                {sec.rows === null && <span className="text-ink-500">unknown</span>}
                {sec.rows?.slice(0, 3).map((r) => (
                  <div key={r.key} className="truncate">
                    {r.issue ? (
                      <button className="lnk num block max-w-full truncate text-left" onClick={() => onOpenIssue(r.issue!)}>
                        {r.text}
                      </button>
                    ) : (
                      r.text
                    )}
                  </div>
                ))}
                {sec.rows && sec.rows.length > 3 && (
                  <div className="text-ink-500">+{sec.rows.length - 3} more</div>
                )}
              </dd>
            </div>
          ))}
        </dl>
      )}
      {load.kind === "ok" && load.view.backlog > 0 && (
        <p className="text-micro text-warn mt-1.5">
          {load.view.backlog} report{load.view.backlog === 1 ? "" : "s"} still queued for the master.
        </p>
      )}
    </section>
  );
}
