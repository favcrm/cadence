import type { AgentsPayload, Health, IssueCard, Project } from "../types";
import Card from "./Card";

const COLS: [string, string, number?][] = [
  ["backlog", "Backlog"],
  ["ready", "Ready"],
  ["doing", "Doing", 3],
  ["review", "Review"],
  ["done", "Done"],
];

const COL_DOT: Record<string, string> = {
  backlog: "bg-ink-600",
  ready: "bg-ink-400",
  doing: "bg-info",
  review: "bg-warn",
  done: "bg-ok",
};

const AGENT_DOT: Record<string, string> = {
  busy: "bg-info",
  idle: "bg-ok",
  stopped: "bg-ink-600",
  attention: "bg-fail",
};

interface Props {
  issues: IssueCard[];
  projects: Project[];
  agents: AgentsPayload | null;
  health: Health | null;
  project: string;
  query: string;
  onQuery: (q: string) => void;
  onOpen: (id: string) => void;
}

export default function Board({
  issues,
  projects,
  agents,
  health,
  project,
  query,
  onQuery,
  onOpen,
}: Props) {
  const visible = issues.filter(
    (t) =>
      !t.container &&
      t.status !== "dropped" &&
      (project === "all" || t.project === project) &&
      (!query ||
        (t.id + t.title + (t.owner ?? ""))
          .toLowerCase()
          .includes(query.toLowerCase())),
  );
  const title =
    project === "all"
      ? "All projects"
      : projects.find((p) => p.key === project)?.key ?? project;

  // issue id → agent aliases currently running a turn that names it
  const busy = new Map<string, string[]>();
  for (const a of agents?.agents ?? []) {
    for (const id of a.on) {
      busy.set(id, [...(busy.get(id) ?? []), a.alias]);
    }
  }
  const titleOf = new Map(issues.map((i) => [i.id, i.title]));

  const totals = agents?.totals;
  const activeAgents = (agents?.agents ?? []).filter(
    (a) => a.running > 0 || a.fenced,
  );
  const idleCount = (agents?.agents ?? []).filter(
    (a) => a.state === "idle" && !a.fenced,
  ).length;
  const stoppedCount = (agents?.agents ?? []).filter(
    (a) => a.state === "stopped" && !a.fenced,
  ).length;

  return (
    <main className="px-4 lg:px-8 pt-6 pb-9 max-w-[106rem] w-full">
      {health && !health.pm_present && (
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 mb-5 reveal">
          <span className="chip bg-warn/10 text-warn">no pm dir</span>
          <span className="text-secondary text-ink-400">
            Nothing at {health.pm_dir ?? "~/pm"} yet —{" "}
            <span className="num">cadence issue init</span> creates it.
          </span>
        </div>
      )}

      <div
        className="flex flex-wrap items-end gap-x-3 gap-y-3 mb-4 reveal"
        style={{ animationDelay: "40ms" }}
      >
        <h1 className="text-section font-semibold text-ink-100 leading-tight">
          {title}
        </h1>
        <span className="kicker">
          {visible.length} issues · {visible.filter((t) => t.blocked).length}{" "}
          blocked ·{" "}
          {visible.filter((t) => t.status_source !== "file").length} derived
        </span>
        <input
          value={query}
          onChange={(e) => onQuery(e.target.value)}
          className="field ml-auto w-full sm:w-64"
          placeholder="Filter issues by id, title or owner"
        />
      </div>

      <section
        className="card mb-4 px-4 py-2.5 flex flex-wrap items-center gap-x-5 gap-y-2 reveal"
        style={{ animationDelay: "80ms" }}
        aria-label="Runtime"
      >
        <div className="flex items-baseline gap-x-5 gap-y-1 flex-wrap">
          <span className="flex items-baseline gap-1.5">
            <span className="slabel">running</span>
            <span className="num text-secondary text-ink-100">
              {totals?.running ?? "—"}
            </span>
          </span>
          <span className="flex items-baseline gap-1.5">
            <span className="slabel">queued</span>
            <span className="num text-secondary text-ink-100">
              {totals?.queued ?? "—"}
            </span>
          </span>
          <span className="flex items-baseline gap-1.5">
            <span className="slabel">fenced</span>
            <span
              className={`num text-secondary ${
                (totals?.fenced ?? 0) > 0 ? "text-fail" : "text-ok"
              }`}
            >
              {totals?.fenced ?? "—"}
            </span>
          </span>
          <span className="flex items-baseline gap-1.5">
            <span className="slabel">parked</span>
            <span className="num text-secondary text-ink-100">
              {totals?.parked ?? "—"}
            </span>
          </span>
        </div>
        <span className="hidden md:block w-px h-4 bg-ink-700" />
        <div
          className="flex items-center gap-2 min-w-0 flex-wrap"
          title={
            agents?.daemon === "reachable"
              ? "read from the daemon socket"
              : "daemon socket unreachable"
          }
        >
          <span className="slabel">agents</span>
          <div className="flex flex-wrap gap-1.5">
            {agents?.daemon === "unreachable" && (
              <span className="chip bg-ink-800 !py-[.15rem] text-ink-500">
                <i className="w-1.5 h-1.5 rounded-full bg-ink-600" />
                daemon unreachable
              </span>
            )}
            {activeAgents.map((a) => (
              <span
                key={a.alias}
                className="chip bg-ink-800 !py-[.15rem] text-ink-300"
              >
                <i
                  className={`w-1.5 h-1.5 rounded-full ${
                    a.fenced ? AGENT_DOT.attention : AGENT_DOT.busy
                  }`}
                />
                {a.alias}
                <span className="text-ink-500">
                  {a.fenced ? "fenced" : "busy"}
                </span>
                {a.on.map((id) => (
                  <button
                    key={id}
                    className="lnk"
                    onClick={() => onOpen(id)}
                  >
                    {id}
                  </button>
                ))}
              </span>
            ))}
            {idleCount > 0 && (
              <span
                className="chip bg-ink-800 !py-[.15rem] text-ink-400"
                title={(agents?.agents ?? [])
                  .filter((a) => a.state === "idle" && !a.fenced)
                  .map((a) => a.alias)
                  .join(", ")}
              >
                <i className="w-1.5 h-1.5 rounded-full bg-ok" />
                <span className="num text-ink-200">{idleCount}</span>idle
              </span>
            )}
            {stoppedCount > 0 && (
              <span
                className="chip bg-ink-800 !py-[.15rem] text-ink-400"
                title={(agents?.agents ?? [])
                  .filter((a) => a.state === "stopped" && !a.fenced)
                  .map((a) => a.alias)
                  .join(", ")}
              >
                <i className="w-1.5 h-1.5 rounded-full bg-ink-600" />
                <span className="num text-ink-200">{stoppedCount}</span>stopped
              </span>
            )}
          </div>
        </div>
      </section>

      <div className="grid grid-flow-col auto-cols-[minmax(232px,1fr)] lg:auto-cols-[minmax(0,1fr)] gap-3 overflow-x-auto lg:overflow-visible pb-2">
        {COLS.map(([key, name, wip], ci) => {
          const cards = visible.filter((t) => t.status === key);
          return (
            <section
              key={key}
              className="rounded-lg border border-ink-700 bg-ink-875 min-h-[26rem] flex flex-col reveal"
              style={{ animationDelay: `${120 + ci * 45}ms` }}
            >
              <header className="lg:sticky lg:top-[2.85rem] z-[5] flex items-center gap-2 px-3.5 h-10 border-b border-ink-700 shrink-0 bg-ink-875 rounded-t-lg">
                <i className={`w-1.5 h-1.5 rounded-full ${COL_DOT[key]}`} />
                <h2 className="text-secondary font-semibold text-ink-100">
                  {name}
                </h2>
                <span className="kicker num">
                  {cards.length}
                  {wip ? ` of ${wip} wip` : ""}
                </span>
              </header>
              <div className="p-2.5 space-y-2.5 flex-1">
                {cards.length === 0 ? (
                  <p className="kicker px-1 py-2">empty</p>
                ) : (
                  cards.map((t) => (
                    <Card
                      key={t.id}
                      issue={t}
                      parentTitle={
                        t.parent ? titleOf.get(t.parent) : undefined
                      }
                      busyBy={busy.get(t.id) ?? []}
                      onOpen={onOpen}
                    />
                  ))
                )}
              </div>
            </section>
          );
        })}
      </div>

      <footer className="mt-8 pt-4 border-t border-ink-700 text-label text-ink-500 num">
        source: {health?.pm_dir ?? "~/pm"} issue folders ·{" "}
        /var/www/agent-notes chains · cadence daemon socket — read-only.
      </footer>
    </main>
  );
}
