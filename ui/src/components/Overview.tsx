import { useState } from "react";
import type { MonitorAlert, Monitoring, Overview } from "../types";

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

function when(epoch: number | null | undefined): string {
  if (epoch == null) return "never";
  return new Date(epoch * 1000).toLocaleString();
}

const MONITOR_STATE_CHIP: Record<string, string> = {
  active: "bg-ok/15 text-ok",
  degraded: "bg-fail/10 text-fail",
  stale: "bg-warn/10 text-warn",
  stopped: "bg-ink-800 text-ink-400",
  unavailable: "bg-warn/10 text-warn",
  off: "bg-ink-800 text-ink-400",
};

function MonitoringView({
  data,
  readOnly,
  onAck,
}: {
  data: Monitoring;
  readOnly: boolean;
  onAck: (monitor: string, seq: number) => void;
}) {
  return (
    <section>
      <div className="slabel mb-2">coordinator</div>
      <div className="card px-4 py-3.5 space-y-3">
        <div className="flex flex-wrap items-center gap-2">
          <span
            className={`chip ${MONITOR_STATE_CHIP[data.state] ?? "bg-ink-800 text-ink-400"}`}
          >
            monitor {data.state}
          </span>
          <span className="text-label text-ink-400">
            last successful scan: {when(data.last_success_at)}
          </span>
          <span className="text-micro text-ink-500">
            next reconciliation: {when(data.next_check_at)}
          </span>
        </div>

        <div className="text-micro text-ink-500">
          local UI visibility only — external push delivery is unconfigured
        </div>

        {!data.available && (
          <div className="text-label text-warn">
            monitor RPC unavailable; persistent monitor health cannot be confirmed
          </div>
        )}
        {data.errors.map((e, i) => (
          <div key={`${e.monitor ?? "monitor"}-${i}`} className="text-label text-fail">
            {e.monitor ? `${e.monitor}: ` : ""}{e.error}
          </div>
        ))}

        {data.monitors.length > 0 && (
          <div className="space-y-1 border-t border-ink-700/60 pt-2">
            {data.monitors.map((monitor) => (
              <div
                key={monitor.id}
                className="flex flex-wrap items-center gap-x-2 gap-y-1 text-micro"
              >
                <span
                  className={`chip !py-[.15rem] ${MONITOR_STATE_CHIP[monitor.monitoring] ?? "bg-ink-800 text-ink-400"}`}
                >
                  {monitor.monitoring}
                </span>
                <span className="num text-ink-300">{monitor.id}</span>
                <span className="text-ink-500">owner {monitor.owner}</span>
                <span className="text-ink-600">
                  dispatch {monitor.auto_dispatch_enabled ? "automatic opt-in" : monitor.dispatch_enabled ? "manual" : "off"}
                </span>
                <span className="text-ink-500">
                  scan {when(monitor.last_success_at)} · last check {when(monitor.last_check_at)}
                </span>
                <span className="text-ink-600">
                  heartbeat {when(monitor.heartbeat_at)} · coverage {monitor.coverage.join(", ") || "none"}
                </span>
                {monitor.error && <span className="text-fail">{monitor.error}</span>}
              </div>
            ))}
          </div>
        )}

        {data.monitors.length === 0 && data.available && (
          <div className="text-label text-ink-500">
            no monitor registration is active; autonomous reconciliation is stopped
          </div>
        )}

        {data.alerts.length > 0 && (
          <div className="space-y-2 border-t border-ink-700/60 pt-3">
            <div className="flex items-center justify-between">
              <span className="slabel">actionable alerts</span>
              <span className="num text-micro text-ink-500">
                {data.open_alerts} open · {data.alerts.length} recorded
              </span>
            </div>
            {data.alerts.map((alert) => (
              <MonitorAlertView
                key={`${alert.monitor}-${alert.seq}`}
                alert={alert}
                readOnly={readOnly}
                onAck={onAck}
              />
            ))}
          </div>
        )}
      </div>
    </section>
  );
}

function MonitorAlertView({
  alert,
  readOnly,
  onAck,
}: {
  alert: MonitorAlert;
  readOnly: boolean;
  onAck: (monitor: string, seq: number) => void;
}) {
  const open = alert.state === "open";
  const stateLabel = alert.state === "resolved" ? "resolved" : open ? "open" : "acknowledged";
  return (
    <div className="rounded border border-ink-700/70 bg-ink-875 px-3 py-2.5 space-y-1.5">
      <div className="flex flex-wrap items-center gap-2">
        <span className={`chip !py-[.15rem] ${open ? "bg-warn/10 text-warn" : "bg-ink-800 text-ink-500"}`}>
          {stateLabel}
        </span>
        <span className="chip !py-[.15rem] bg-ink-800 text-ink-300">{alert.kind}</span>
        <span className="num text-micro text-ink-500">{age(alert.age_secs)}</span>
        <span className="num text-micro text-ink-500">
          {alert.project} · {alert.monitor} · {alert.task ?? "task unknown"}
        </span>
        {open && (
          <button
            className="chip ml-auto bg-accent/10 text-accent hover:bg-accent/20 disabled:opacity-50"
            disabled={readOnly}
            title={readOnly ? "board is read-only" : "acknowledge this durable monitor alert"}
            onClick={() => onAck(alert.monitor, alert.seq)}
          >
            ack
          </button>
        )}
      </div>
      <div className="text-label text-ink-200">{alert.next_action}</div>
      <div className="text-micro text-ink-500">
        next owner <span className="text-ink-300">{alert.next_owner}</span> · {alert.authority}
      </div>
      <div className="text-micro text-ink-600">
        evidence event {alert.event_seq} · {alert.fingerprint}
      </div>
    </div>
  );
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

export default function OverviewView({
  data,
  readOnly,
  onAck,
}: {
  data: Overview | null;
  readOnly: boolean;
  onAck: (monitor: string, seq: number) => void;
}) {
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

      {data.monitoring && (
        <MonitoringView data={data.monitoring} readOnly={readOnly} onAck={onAck} />
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
