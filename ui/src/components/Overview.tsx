import { useState } from "react";
import type { Overview } from "../types";

const KIND_CHIP: Record<string, string> = {
  merge: "bg-ok/15 text-ok",
  approval: "bg-warn/10 text-warn",
  fenced: "bg-fail/10 text-fail",
  stalled: "bg-warn/10 text-warn",
  drift: "bg-accent/10 text-accent",
  pr_no_verdict: "bg-info/10 text-info",
  review_no_pr: "bg-ink-800 text-ink-300",
  blocked_ready: "bg-accent/10 text-accent",
  ci_red: "bg-fail/10 text-fail",
  inbox_unread: "bg-warn/10 text-warn",
  tracker_behind: "bg-ink-800 text-ink-400",
};

const KIND_LABEL: Record<string, string> = {
  merge: "merge",
  approval: "approval",
  fenced: "fenced",
  stalled: "stalled",
  drift: "drift",
  pr_no_verdict: "no verdict",
  review_no_pr: "review",
  blocked_ready: "unblocked",
  ci_red: "ci red",
  inbox_unread: "inbox",
  tracker_behind: "behind",
};

function age(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}

function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      className="chip bg-ink-800 !py-[.15rem] text-ink-400 hover:text-accent shrink-0"
      title={`copy: ${text}`}
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(text);
          setCopied(true);
          window.setTimeout(() => setCopied(false), 1400);
        } catch {
          // Clipboard needs a secure context — the command stays
          // visible in the title for hand-copying.
        }
      }}
    >
      {copied ? "copied" : "copy"}
    </button>
  );
}

export default function OverviewView({ data }: { data: Overview | null }) {
  if (!data) {
    return (
      <div className="px-4 lg:px-8 pt-4 text-label text-ink-500">
        overview unavailable — the board server could not build the view
      </div>
    );
  }
  const drift = data.drift;
  return (
    <div className="px-4 lg:px-8 pt-4 pb-10 space-y-5 max-w-[68rem]">
      {(data.github.state === "unavailable" || !data.daemon.reachable) && (
        <div className="card border-warn/40 px-4 py-3 text-secondary text-warn">
          {data.github.state === "unavailable" && (
            <div>github unavailable — PR and CI rows are missing</div>
          )}
          {!data.daemon.reachable && (
            <div>daemon unreachable — agent, approval and drift rows are missing</div>
          )}
        </div>
      )}

      <section>
        <div className="slabel mb-2">needs me</div>
        {data.needs_me.length === 0 ? (
          <div className="card px-4 py-6 text-center text-label text-ink-500">
            nothing waiting on a human
          </div>
        ) : (
          <div className="space-y-1.5">
            {data.needs_me.map((n, i) => (
              <div
                key={i}
                className="card px-3.5 py-2.5 flex flex-wrap items-center gap-x-3 gap-y-1.5"
              >
                <span className={`chip ${KIND_CHIP[n.kind] ?? "bg-ink-800 text-ink-400"}`}>
                  {KIND_LABEL[n.kind] ?? n.kind}
                </span>
                <span className="num text-label text-ink-500 w-9 shrink-0">
                  {age(n.age)}
                </span>
                <span className="min-w-0 flex-1 basis-[12rem] text-label text-ink-200">
                  {n.link ? (
                    <a
                      href={n.link}
                      target="_blank"
                      rel="noreferrer"
                      className="hover:text-accent"
                    >
                      {n.title}
                    </a>
                  ) : (
                    n.title
                  )}
                  {n.project && (
                    <span className="text-ink-500"> · {n.project}</span>
                  )}
                </span>
                <span className="flex basis-full sm:basis-auto items-center gap-1.5 min-w-0">
                  <code className="num text-micro text-ink-400 truncate max-w-[16rem] sm:max-w-[26rem]">
                    {n.command}
                  </code>
                  <CopyButton text={n.command} />
                </span>
              </div>
            ))}
          </div>
        )}
      </section>

      <section>
        <div className="slabel mb-2">deploy drift</div>
        <div className="card px-4 py-3.5">
          {drift.known ? (
            drift.count === 0 ? (
              <div className="text-label text-ink-300">
                <span className="text-ok">up to date</span> — {drift.project} is
                running the latest on {drift.ref}
              </div>
            ) : (
              <div>
                <div className="text-label text-ink-200">
                  <span className="text-warn font-semibold">
                    {drift.count} commit{drift.count === 1 ? "" : "s"}
                  </span>{" "}
                  merged on {drift.ref} past build{" "}
                  <code className="num text-ink-400">
                    {drift.build_commit?.slice(0, 10)}
                  </code>{" "}
                  ({drift.project})
                </div>
                <ul className="mt-2 space-y-0.5">
                  {(drift.commits ?? []).map((c, i) => (
                    <li key={i} className="text-micro text-ink-400 truncate">
                      {c.subject}
                      {c.pr ? <span className="text-ink-500"> #{c.pr}</span> : null}
                    </li>
                  ))}
                </ul>
                {drift.held && (
                  <div className="text-micro text-ink-500 mt-2">{drift.held}</div>
                )}
              </div>
            )
          ) : (
            <div className="text-label text-ink-500">
              {drift.reason ?? "cannot tell"}
            </div>
          )}
        </div>
      </section>

      {data.projects.length > 0 && (
        <section>
          <div className="slabel mb-2">projects</div>
          <div className="card divide-y divide-ink-700/60">
            {data.projects.map((p) => {
              const counts = Object.entries(p.open_by_status)
                .sort(([a], [b]) => a.localeCompare(b))
                .map(([k, v]) => `${k}:${v}`)
                .join("  ");
              return (
                <div key={p.key} className="px-4 py-2.5 flex items-baseline gap-3">
                  <span className="text-label font-semibold text-ink-100 w-28 shrink-0 truncate">
                    {p.key}
                  </span>
                  <span className="num text-label text-ink-400 min-w-0 flex-1">
                    {counts || "no open issues"}
                  </span>
                  {p.oldest_review_age != null && (
                    <span className="num text-micro text-warn shrink-0">
                      oldest review {age(p.oldest_review_age)}
                    </span>
                  )}
                </div>
              );
            })}
          </div>
        </section>
      )}
    </div>
  );
}
