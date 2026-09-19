import { useState } from "react";
import { api, type WriteResp } from "../api";
import type { AgentsPayload, Health, IssueCard, Project } from "../types";
import Card, { noDragReason } from "./Card";

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
  readOnly: boolean;
  actor: string;
  onQuery: (q: string) => void;
  onOpen: (id: string) => void;
  onMove: (issue: IssueCard, status: string) => void;
  onCreated: (resp: WriteResp, verb: string) => void;
  onError: (e: unknown, verb: string) => void;
  onAgents: () => void;
}

/// Quick-add at the top of Backlog: title + project + priority, Enter
/// creates, Escape cancels.
function QuickAdd({
  projects,
  project,
  onCreated,
  onError,
}: {
  projects: Project[];
  project: string;
  onCreated: (resp: WriteResp, verb: string) => void;
  onError: (e: unknown, verb: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [title, setTitle] = useState("");
  const [priority, setPriority] = useState("P2");
  const [selProject, setSelProject] = useState("");
  const [busy, setBusy] = useState(false);
  const projectKey =
    project === "all" ? selProject || (projects[0]?.key ?? "") : project;

  if (!open) {
    return (
      <button
        onClick={() => setOpen(true)}
        className="w-full rounded border border-dashed border-ink-600 px-3 py-2 text-left text-label text-ink-500 hover:border-accent/60 hover:text-accent transition-colors"
      >
        + new issue
      </button>
    );
  }
  const submit = () => {
    const t = title.trim();
    if (!t || !projectKey || busy) return;
    setBusy(true);
    api
      .create({ project: projectKey, title: t, priority })
      .then((resp) => {
        onCreated(resp, `${resp.card.id} created`);
        setTitle("");
        setOpen(false);
      })
      .catch((e) => onError(e, "create"))
      .finally(() => setBusy(false));
  };
  return (
    <div className="card p-2.5 space-y-2 border-accent/40">
      <input
        autoFocus
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") submit();
          if (e.key === "Escape") {
            setTitle("");
            setOpen(false);
          }
        }}
        className="field w-full"
        placeholder={`title — creates in ${projectKey}`}
      />
      <div className="flex gap-1.5">
        {project === "all" ? (
          <select
            value={projectKey}
            onChange={(e) => setSelProject(e.target.value)}
            className="field !h-7 flex-1 text-label"
            aria-label="project"
          >
            {projects.map((p) => (
              <option key={p.key} value={p.key}>
                {p.key}
              </option>
            ))}
          </select>
        ) : (
          <span className="chip bg-ink-800 text-ink-400 self-center">
            {projectKey}
          </span>
        )}
        <select
          value={priority}
          onChange={(e) => setPriority(e.target.value)}
          className="field !h-7 text-label"
          aria-label="priority"
        >
          {["P0", "P1", "P2", "P3"].map((p) => (
            <option key={p}>{p}</option>
          ))}
        </select>
        <button
          onClick={submit}
          disabled={!title.trim() || busy}
          className="h-7 px-2.5 rounded bg-accent text-ink-950 text-label font-medium disabled:opacity-40"
        >
          add
        </button>
      </div>
    </div>
  );
}

export default function Board({
  issues,
  projects,
  agents,
  health,
  project,
  query,
  readOnly,
  actor,
  onQuery,
  onOpen,
  onMove,
  onCreated,
  onError,
  onAgents,
}: Props) {
  const [over, setOver] = useState<string | null>(null);
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
  const fencedAgents = (agents?.agents ?? []).filter((a) => a.fenced);
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

      {fencedAgents.length > 0 && (
        <div className="card mb-4 px-4 py-3 flex flex-wrap items-center gap-x-4 gap-y-2 border-fail/40 reveal">
          <span className="chip bg-fail/10 text-fail">
            {fencedAgents.length} fenced
          </span>
          <span className="text-secondary text-ink-300 min-w-0">
            {fencedAgents.map((a) => a.alias).join(", ")} —{" "}
            {fencedAgents[0]?.recovery ??
              "outcomes are uncertain until an operator reconciles"}
          </span>
          <button
            onClick={onAgents}
            className="chip bg-fail/10 text-fail hover:bg-fail/20 transition-colors ml-auto"
          >
            open Agents →
          </button>
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
          {(totals?.inboxes ?? 0) > 0 && (
            <span
              className="flex items-baseline gap-1.5"
              title="inbox endpoints are mailboxes, not workers"
            >
              <span className="slabel">inboxes</span>
              <span className="num text-secondary text-ink-400">
                {totals?.inboxes}
              </span>
            </span>
          )}
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

      <div className="grid grid-flow-col auto-cols-[minmax(232px,78vw)] lg:auto-cols-[minmax(0,1fr)] gap-3 overflow-x-auto lg:overflow-visible pb-2 snap-x snap-mandatory lg:snap-none">
        {COLS.map(([key, name, wip], ci) => {
          const cards = visible.filter((t) => t.status === key);
          return (
            <section
              key={key}
              className={`snap-start rounded-lg border bg-ink-875 min-h-[26rem] flex flex-col reveal transition-colors ${
                over === key ? "border-accent/60" : "border-ink-700"
              }`}
              style={{ animationDelay: `${120 + ci * 45}ms` }}
              onDragOver={(e) => {
                if (readOnly) return;
                e.preventDefault();
                e.dataTransfer.dropEffect = "move";
                setOver(key);
              }}
              onDragLeave={(e) => {
                if (!e.currentTarget.contains(e.relatedTarget as Node)) {
                  setOver((o) => (o === key ? null : o));
                }
              }}
              onDrop={(e) => {
                e.preventDefault();
                setOver(null);
                if (readOnly) return;
                const id = e.dataTransfer.getData("text/plain");
                const issue = issues.find((t) => t.id === id);
                if (!issue || issue.status === key) return;
                const reason = noDragReason(issue);
                if (reason) {
                  onError(new Error(`${id}: ${reason}`), "move");
                  return;
                }
                onMove(issue, key);
              }}
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
                {key === "backlog" && !readOnly && (
                  <QuickAdd
                    projects={projects}
                    project={project}
                    onCreated={onCreated}
                    onError={onError}
                  />
                )}
                {cards.length === 0 && key !== "backlog" ? (
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
                      canDrag={!readOnly}
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
        /var/www/agent-notes chains · cadence daemon socket ·{" "}
        {readOnly
          ? "read-only — writes are disabled."
          : `writes commit as ${actor}.`}
      </footer>
    </main>
  );
}
