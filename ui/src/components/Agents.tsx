import { useEffect, useState } from "react";
import { api } from "../api";
import { fmtTime } from "../fmt";
import type { Agent, AgentDetail, AgentsPayload } from "../types";

const STATE_CHIP: Record<string, string> = {
  busy: "bg-info/10 text-info",
  idle: "bg-ok/10 text-ok",
  stopped: "bg-ink-800 text-ink-400",
  attention: "bg-fail/10 text-fail",
  inbox: "bg-ink-800 text-ink-500",
};

const STATE_DOT: Record<string, string> = {
  busy: "bg-info",
  idle: "bg-ok",
  stopped: "bg-ink-600",
  attention: "bg-fail",
  inbox: "bg-ink-700",
};

/// Table order: fenced first, then working, idle, stopped, mailboxes.
function rank(a: Agent): number {
  if (a.fenced) return 0;
  if (a.inbox) return 4;
  if (a.running > 0 || a.queued > 0) return 1;
  if (a.state === "idle") return 2;
  return 3;
}

function stateLabel(a: Agent): string {
  if (a.fenced) return "attention";
  if (a.inbox) return "inbox";
  if (a.running > 0) return "busy";
  return a.state;
}

/// Compact silence label for the badge — "45s", "34m", "1h 5m".
function fmtSilence(secs: number): string {
  if (secs < 60) return `${Math.floor(secs)}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
}

/// A fence's recovery path — the daemon's own error text already names
/// `agent unfence` / `message reconcile`; render it verbatim as code.
function RecoveryBlock({ text }: { text: string }) {
  return (
    <pre className="mt-1.5 whitespace-pre-wrap rounded border border-fail/30 bg-fail/5 px-2.5 py-2 text-micro text-fail/90 font-mono">
      {text}
    </pre>
  );
}

function AgentDrawer({
  alias,
  onClose,
  onOpenIssue,
}: {
  alias: string;
  onClose: () => void;
  onOpenIssue: (id: string) => void;
}) {
  const [detail, setDetail] = useState<AgentDetail | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    setDetail(null);
    setErr(null);
    let live = true;
    api
      .agent(alias)
      .then((d) => live && setDetail(d))
      .catch((e) => live && setErr(String(e.message ?? e)));
    return () => {
      live = false;
    };
  }, [alias]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  const a = detail?.agent;
  const caps = (a?.capabilities ?? {}) as Record<string, unknown>;
  const capList = Object.entries(caps).filter(([, v]) => v === true || typeof v === "string");

  return (
    <>
      <div className="fixed inset-0 bg-ink-950/70 z-20" onClick={onClose} />
      <aside
        className="drawer fixed top-0 right-0 h-full w-full sm:w-[34rem] bg-ink-875 border-l border-ink-700 z-30 flex flex-col"
        aria-label="Agent detail"
      >
        <header className="px-5 pt-4 pb-4 border-b border-ink-700 flex items-start gap-3 shrink-0">
          <div className="min-w-0 flex-1">
            <div className="num text-label text-ink-500">
              {alias}
              {a ? ` · ${a.provider} · ${a.endpoint_kind} · ${a.state}` : ""}
            </div>
            <h2 className="text-drawer font-semibold text-ink-100 leading-tight mt-1">
              {a?.role ?? "agent"}
            </h2>
          </div>
          <button
            aria-label="Close"
            onClick={onClose}
            className="closebtn ml-auto shrink-0 w-8 h-8 grid place-items-center rounded border border-ink-600 text-ink-300 bg-ink-850"
          >
            <svg
              width="12"
              height="12"
              viewBox="0 0 12 12"
              stroke="currentColor"
              strokeWidth="1.5"
              fill="none"
              style={{ pointerEvents: "none" }}
            >
              <path d="M2 2l8 8M10 2l-8 8" />
            </svg>
          </button>
        </header>

        <div className="flex-1 overflow-y-auto px-5 py-5 space-y-6">
          {err && <p className="text-secondary text-fail">{err}</p>}
          {detail && a && (
            <>
              {detail.fenced && detail.recovery && (
                <section>
                  <div className="flex items-baseline gap-2 mb-2">
                    <h3 className="text-cardtitle font-semibold text-fail">
                      Fenced
                    </h3>
                    <span className="kicker">recovery is an operator command</span>
                  </div>
                  <RecoveryBlock text={detail.recovery} />
                </section>
              )}

              <section>
                <div className="flex items-baseline gap-2 mb-2">
                  <h3 className="text-cardtitle font-semibold text-ink-100">
                    Identity
                  </h3>
                </div>
                <dl className="grid grid-cols-[7rem_1fr] gap-y-1.5 text-label">
                  {(
                    [
                      ["thread", a.thread_id],
                      ["session", a.session_id],
                      ["endpoint", a.endpoint],
                      ["pid", a.pid],
                      ["model", a.model],
                      ["generation", a.generation],
                      ["cwd", a.cwd],
                      ["sandbox", a.sandbox],
                    ] as [string, unknown][]
                  )
                    .filter(([, v]) => v !== null && v !== undefined && v !== "")
                    .map(([k, v]) => (
                      <div key={k} className="contents">
                        <dt className="slabel">{k}</dt>
                        <dd className="num text-ink-300 truncate" title={String(v)}>
                          {String(v)}
                        </dd>
                      </div>
                    ))}
                  {detail.resume && (
                    <div className="contents">
                      <dt className="slabel">resume</dt>
                      <dd>
                        <code className="num text-micro text-accent bg-accent/10 rounded px-1.5 py-0.5">
                          {detail.resume}
                        </code>
                      </dd>
                    </div>
                  )}
                  {Object.entries(a.params ?? {}).map(([k, v]) => (
                    <div key={k} className="contents">
                      <dt className="slabel" title={`param ${k}`}>
                        {k}
                      </dt>
                      <dd
                        className="num text-ink-300 truncate"
                        title={typeof v === "string" ? v : JSON.stringify(v)}
                      >
                        {typeof v === "string" ? v : JSON.stringify(v)}
                      </dd>
                    </div>
                  ))}
                </dl>
              </section>

              {(detail.tasks?.length ?? 0) > 0 && (
                <section>
                  <div className="flex items-baseline gap-2 mb-2">
                    <h3 className="text-cardtitle font-semibold text-ink-100">
                      Tasks
                    </h3>
                    <span className="kicker">assigned in flight</span>
                  </div>
                  <ul className="border border-ink-700 rounded-lg divide-y divide-ink-700/80 bg-ink-850">
                    {detail.tasks!.map((t) => (
                      <li
                        key={t}
                        className="flex items-center gap-2.5 px-3 py-2.5"
                      >
                        <span className="num text-label text-ink-200">{t}</span>
                      </li>
                    ))}
                  </ul>
                </section>
              )}

              {(detail.on?.length ?? 0) > 0 && (
                <section>
                  <div className="flex items-baseline gap-2 mb-2">
                    <h3 className="text-cardtitle font-semibold text-ink-100">
                      Issues
                    </h3>
                    <span className="kicker">bound through the job</span>
                  </div>
                  <div className="flex flex-wrap gap-1.5">
                    {detail.on!.map((id) => (
                      <button
                        key={id}
                        className="lnk"
                        onClick={() => onOpenIssue(id)}
                      >
                        {id}
                      </button>
                    ))}
                  </div>
                </section>
              )}

              {(detail.running.length > 0 || detail.queued > 0) && (
                <section>
                  <div className="flex items-baseline gap-2 mb-2">
                    <h3 className="text-cardtitle font-semibold text-ink-100">
                      Messages
                    </h3>
                    <span className="kicker">
                      {detail.running.length} running · {detail.queued} queued ·{" "}
                      {detail.unknown} unknown
                    </span>
                  </div>
                  <ul className="border border-ink-700 rounded-lg divide-y divide-ink-700/80 bg-ink-850">
                    {detail.running.map((m) => (
                      <li
                        key={m.id}
                        className="flex items-center gap-2.5 px-3 py-2.5"
                      >
                        <i className="w-1.5 h-1.5 rounded-full bg-info" />
                        <span className="num text-label text-ink-200">{m.id}</span>
                        {m.turn_id && (
                          <span
                            className="num text-micro text-ink-500"
                            title="turn id"
                          >
                            {m.turn_id}
                          </span>
                        )}
                        {m.task && (
                          <span className="num text-micro text-ink-500">
                            task {m.task}
                          </span>
                        )}
                        {m.created && (
                          <span className="num text-micro text-ink-500 ml-auto">
                            {fmtTime(m.created)}
                          </span>
                        )}
                      </li>
                    ))}
                  </ul>
                </section>
              )}

              {capList.length > 0 && (
                <section>
                  <div className="flex items-baseline gap-2 mb-2">
                    <h3 className="text-cardtitle font-semibold text-ink-100">
                      Capabilities
                    </h3>
                    <span className="kicker">from the registry</span>
                  </div>
                  <div className="flex flex-wrap gap-1.5">
                    {capList.map(([k, v]) => (
                      <span
                        key={k}
                        className="chip bg-ink-800 text-ink-300"
                        title={typeof v === "string" ? v : k}
                      >
                        {typeof v === "string" ? `${k}: ${v}` : k}
                      </span>
                    ))}
                  </div>
                </section>
              )}

              <section>
                <div className="flex items-baseline gap-2 mb-2">
                  <h3 className="text-cardtitle font-semibold text-ink-100">
                    Events
                  </h3>
                  <span className="kicker">last {detail.events.length}</span>
                </div>
                {detail.events.length ? (
                  <ul className="border border-ink-700 rounded-lg divide-y divide-ink-700/80 bg-ink-850">
                    {detail.events.map((e) => (
                      <li
                        key={e.seq}
                        className="flex items-baseline gap-2.5 px-3 py-2"
                      >
                        <span className="num text-micro text-ink-500 w-8 shrink-0">
                          #{e.seq}
                        </span>
                        <span className="num text-label text-ink-200">
                          {e.kind}
                        </span>
                        {(e.task_id || e.job_id) && (
                          <span className="num text-micro text-ink-500">
                            {e.task_id ?? e.job_id}
                          </span>
                        )}
                        <span className="num text-micro text-ink-500 ml-auto shrink-0">
                          {fmtTime(e.at)}
                        </span>
                      </li>
                    ))}
                  </ul>
                ) : (
                  <p className="text-secondary text-ink-500">No events yet.</p>
                )}
              </section>
            </>
          )}
        </div>
      </aside>
    </>
  );
}

export default function Agents({
  payload,
  onOpenIssue,
}: {
  payload: AgentsPayload | null;
  onOpenIssue: (id: string) => void;
}) {
  const [open, setOpen] = useState<string | null>(null);
  const agents = (payload?.agents ?? [])
    .slice()
    .sort((a, b) => rank(a) - rank(b) || a.alias.localeCompare(b.alias));
  const totals = payload?.totals;

  return (
    <main className="px-4 lg:px-8 pt-6 pb-9 max-w-[106rem] w-full">
      <div
        className="flex flex-wrap items-end gap-x-3 gap-y-3 mb-4 reveal"
        style={{ animationDelay: "40ms" }}
      >
        <h1 className="text-section font-semibold text-ink-100 leading-tight">
          Agents
        </h1>
        <span className="kicker">
          {agents.length} registered · {totals?.fenced ?? 0} fenced ·{" "}
          {totals?.queued ?? 0} queued
        </span>
      </div>

      {payload?.daemon === "unreachable" && (
        <div className="card mb-4 px-4 py-3 text-secondary text-warn border-warn/40">
          daemon unreachable — the table below is the last known state.
        </div>
      )}

      {/* Phone: stacked agent cards — the table's columns don't fit 390px. */}
      <div className="sm:hidden space-y-2.5 reveal" style={{ animationDelay: "80ms" }}>
        {agents.map((a) => {
          const st = stateLabel(a);
          return (
            <article
              key={a.alias}
              role="button"
              tabIndex={0}
              onClick={() => setOpen(a.alias)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  setOpen(a.alias);
                }
              }}
              className={`card w-full text-left p-3.5 space-y-2 cursor-pointer ${
                a.fenced ? "border-fail/40" : ""
              }`}
            >
              <div className="flex items-center gap-2">
                <i className={`w-1.5 h-1.5 rounded-full ${STATE_DOT[st] ?? STATE_DOT.stopped}`} />
                <span className="num text-label text-ink-100 font-medium">
                  {a.alias}
                </span>
                <span className={`chip ml-auto ${STATE_CHIP[st] ?? STATE_CHIP.stopped}`}>
                  {st}
                </span>
              </div>
              {a.fenced && <RecoveryBlock text={a.recovery ?? "fenced"} />}
              <div className="grid grid-cols-[5.4rem_1fr] gap-y-1 text-label">
                <span className="slabel">provider</span>
                <span className="num text-ink-300">
                  {a.provider}/{a.endpoint_kind}
                </span>
                <span className="slabel">group</span>
                <span className="num text-ink-300">
                  {a.group_root ? "root" : a.group}
                </span>
                {a.on.length > 0 && (
                  <>
                    <span className="slabel">on issue</span>
                    <span className="flex flex-wrap gap-1.5">
                      {a.on.map((id) => (
                        <span
                          key={id}
                          role="link"
                          className="lnk"
                          onClick={(e) => {
                            e.stopPropagation();
                            onOpenIssue(id);
                          }}
                        >
                          {id}
                        </span>
                      ))}
                    </span>
                  </>
                )}
                <span className="slabel">running</span>
                <span className="num text-ink-300">
                  {a.running}
                  {a.message?.id ? ` · ${a.message.id}` : ""}
                </span>
                <span className="slabel">queued</span>
                <span className={`num ${a.unknown > 0 ? "text-fail" : "text-ink-300"}`}>
                  {a.queued}
                  {a.unknown > 0 ? ` +${a.unknown} unk` : ""}
                </span>
                <span className="slabel">activity</span>
                <span className="num text-ink-300">
                  {a.last_activity ? fmtTime(a.last_activity) : "—"}
                  {a.stalled
                    ? ` · stalled ${fmtSilence(a.silent_secs ?? 0)}`
                    : (a.silent_secs ?? 0) >= 60
                      ? ` · silent ${fmtSilence(a.silent_secs!)}`
                      : ""}
                </span>
                {(a.fenced || a.resume) && (
                  <>
                    <span className="slabel">recovery</span>
                    <span>
                      {a.fenced ? (
                        <span className="chip bg-fail/10 text-fail">reconcile</span>
                      ) : (
                        <code className="num text-micro text-ink-400" title={a.resume_hint ?? "resume command"}>
                          {a.resume}
                        </code>
                      )}
                    </span>
                  </>
                )}
              </div>
            </article>
          );
        })}
        {agents.length === 0 && (
          <div className="card p-8 text-center text-ink-500">
            No agents registered.
          </div>
        )}
      </div>

      <div className="hidden sm:block card overflow-hidden reveal" style={{ animationDelay: "80ms" }}>
        <table className="w-full text-label">
          <thead>
            <tr className="border-b border-ink-700 text-left">
              <th className="slabel font-normal px-4 py-2.5">agent</th>
              <th className="slabel font-normal px-3 py-2.5">provider</th>
              <th className="slabel font-normal px-3 py-2.5">state</th>
              <th className="slabel font-normal px-3 py-2.5">group</th>
              <th className="slabel font-normal px-3 py-2.5">on issue</th>
              <th className="slabel font-normal px-3 py-2.5">running</th>
              <th className="slabel font-normal px-3 py-2.5">queued</th>
              <th className="slabel font-normal px-3 py-2.5">last activity</th>
              <th className="slabel font-normal px-3 py-2.5">recovery</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-ink-700/70">
            {agents.map((a) => {
              const st = stateLabel(a);
              return (
                <tr
                  key={a.alias}
                  onClick={() => setOpen(a.alias)}
                  className={`cursor-pointer hover:bg-ink-850 transition-colors ${
                    a.fenced ? "bg-fail/[.04]" : ""
                  }`}
                >
                  <td className="px-4 py-2.5">
                    <span className="num text-ink-100">{a.alias}</span>
                    {a.fenced && <RecoveryBlock text={a.recovery ?? "fenced"} />}
                  </td>
                  <td className="px-3 py-2.5 align-top">
                    <span className="num text-ink-400">
                      {a.provider}/{a.endpoint_kind}
                    </span>
                  </td>
                  <td className="px-3 py-2.5 align-top">
                    <span className={`chip ${STATE_CHIP[st] ?? STATE_CHIP.stopped}`}>
                      <i className={`w-1.5 h-1.5 rounded-full ${STATE_DOT[st] ?? STATE_DOT.stopped}`} />
                      {st}
                    </span>
                  </td>
                  <td className="px-3 py-2.5 align-top">
                    <span className="num text-ink-400">
                      {a.group_root ? "root" : a.group}
                    </span>
                  </td>
                  <td className="px-3 py-2.5 align-top">
                    <div className="flex flex-wrap gap-1">
                      {a.on.map((id) => (
                        <button
                          key={id}
                          className="lnk"
                          onClick={(e) => {
                            e.stopPropagation();
                            onOpenIssue(id);
                          }}
                        >
                          {id}
                        </button>
                      ))}
                    </div>
                  </td>
                  <td className="px-3 py-2.5 align-top">
                    <span className="num text-ink-200">{a.running}</span>
                    {a.message?.id && (
                      <div
                        className="num text-micro text-ink-500 mt-0.5"
                        title={a.message.turn_id ?? undefined}
                      >
                        {a.message.id}
                      </div>
                    )}
                  </td>
                  <td className="px-3 py-2.5 align-top">
                    <span className={`num ${a.unknown > 0 ? "text-fail" : "text-ink-200"}`}>
                      {a.queued}
                      {a.unknown > 0 ? ` +${a.unknown} unk` : ""}
                    </span>
                  </td>
                  <td className="px-3 py-2.5 align-top">
                    <span className="num text-ink-400">
                      {a.last_activity ? fmtTime(a.last_activity) : "—"}
                    </span>
                    {a.stalled ? (
                      <div>
                        <span className="chip bg-warn/10 text-warn mt-1">
                          stalled {fmtSilence(a.silent_secs ?? 0)}
                        </span>
                      </div>
                    ) : (a.silent_secs ?? 0) >= 60 ? (
                      <div className="num text-micro text-ink-500 mt-0.5">
                        silent {fmtSilence(a.silent_secs!)}
                      </div>
                    ) : null}
                  </td>
                  <td className="px-3 py-2.5 align-top">
                    {a.fenced ? (
                      <span className="chip bg-fail/10 text-fail">
                        reconcile
                      </span>
                    ) : a.resume ? (
                      <code
                        className="num text-micro text-ink-400"
                        title={a.resume_hint ?? "resume command"}
                      >
                        {a.resume}
                      </code>
                    ) : (
                      <span className="num text-ink-600">—</span>
                    )}
                  </td>
                </tr>
              );
            })}
            {agents.length === 0 && (
              <tr>
                <td colSpan={8} className="px-4 py-8 text-center text-ink-500">
                  No agents registered.
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>

      <footer className="mt-8 pt-4 border-t border-ink-700 text-label text-ink-500 num">
        daemon agent_list + agent_show · binding via tasks × jobs.issue_id ·
        fenced agents need `cadence agent unfence` / `message reconcile`.
      </footer>

      {open && (
        <AgentDrawer
          alias={open}
          onClose={() => setOpen(null)}
          onOpenIssue={(id) => {
            setOpen(null);
            onOpenIssue(id);
          }}
        />
      )}
    </main>
  );
}
