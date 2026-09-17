import { useCallback, useEffect, useState } from "react";
import { api } from "./api";
import Board from "./components/Board";
import Drawer from "./components/Drawer";
import Plan from "./components/Plan";
import Sidebar from "./components/Sidebar";
import type { AgentsPayload, Health, IssueCard, Project } from "./types";

export default function App() {
  const [tab, setTab] = useState<"board" | "plan">("board");
  const [project, setProject] = useState("all");
  const [query, setQuery] = useState("");
  const [issues, setIssues] = useState<IssueCard[]>([]);
  const [projects, setProjects] = useState<Project[]>([]);
  const [agents, setAgents] = useState<AgentsPayload | null>(null);
  const [health, setHealth] = useState<Health | null>(null);
  const [openId, setOpenId] = useState<string | null>(null);
  const [failed, setFailed] = useState<string | null>(null);

  // Selection lives in the URL (`?project=cadence&issue=CAD-16`) so a
  // refresh or a pasted link restores the same view.
  useEffect(() => {
    const read = () => {
      const q = new URLSearchParams(location.search);
      setProject(q.get("project") ?? "all");
      setOpenId(q.get("issue"));
    };
    read();
    addEventListener("popstate", read);
    return () => removeEventListener("popstate", read);
  }, []);

  useEffect(() => {
    const q = new URLSearchParams();
    if (project !== "all") q.set("project", project);
    if (openId) q.set("issue", openId);
    const s = q.toString();
    history.replaceState(null, "", location.pathname + (s ? `?${s}` : ""));
  }, [project, openId]);

  const refresh = useCallback(() => {
    api.health().then(setHealth).catch(() => setHealth(null));
    api
      .projects()
      .then((r) => setProjects(r.projects))
      .catch((e) => setFailed(String(e.message ?? e)));
    api
      .issues()
      .then((r) => {
        setIssues(r.issues);
        setFailed(null);
      })
      .catch((e) => setFailed(String(e.message ?? e)));
    api.agents().then(setAgents).catch(() => setAgents(null));
  }, []);

  useEffect(refresh, [refresh]);

  const openIssue = useCallback((id: string) => setOpenId(id), []);

  return (
    <div className="grid lg:grid-cols-[208px_minmax(0,1fr)] min-h-screen bg-ink-900">
      <Sidebar
        tab={tab}
        onTab={setTab}
        project={project}
        onProject={setProject}
        projects={projects}
        total={issues.filter((i) => !i.container).length}
      />

      <div className="min-w-0 flex flex-col">
        <header className="sticky top-0 z-10 h-[2.85rem] flex items-center gap-3 px-4 lg:px-8 border-b border-ink-700 bg-ink-900/95 backdrop-blur">
          <div className="num text-label text-ink-500">
            <span className="text-ink-300">cadence</span> /{" "}
            <span className="text-ink-100">{tab}</span>
          </div>
          <div className="lg:hidden flex gap-1 ml-2">
            <button
              onClick={() => setTab("board")}
              className="chip bg-ink-800 text-ink-200"
            >
              board
            </button>
            <button
              onClick={() => setTab("plan")}
              className="chip bg-ink-800 text-ink-200"
            >
              plan
            </button>
          </div>
          <div className="ml-auto flex items-center gap-2">
            {health && (
              <span
                className={`chip ${
                  health.daemon === "reachable"
                    ? "bg-ink-800 text-ink-400"
                    : "bg-warn/10 text-warn"
                }`}
                title={
                  health.daemon === "reachable"
                    ? "daemon socket reachable"
                    : "daemon socket unreachable — runtime strip is empty"
                }
              >
                daemon {health.daemon}
              </span>
            )}
            <button
              onClick={refresh}
              className="chip bg-ink-800 text-ink-400 hover:text-ink-100 transition-colors"
              title="no live updates in I1 — click to re-read the folders"
            >
              refresh
            </button>
            <span className="hidden sm:inline-flex chip bg-accent/10 text-accent">
              read-only
            </span>
          </div>
        </header>

        {failed && (
          <div className="px-4 lg:px-8 pt-4">
            <div className="card border-fail/40 px-4 py-3 text-secondary text-fail">
              api: {failed}
            </div>
          </div>
        )}

        {tab === "board" ? (
          <Board
            issues={issues}
            projects={projects}
            agents={agents}
            health={health}
            project={project}
            query={query}
            onQuery={setQuery}
            onOpen={openIssue}
          />
        ) : (
          <Plan />
        )}
      </div>

      {openId && (
        <Drawer
          id={openId}
          agents={agents}
          onClose={() => setOpenId(null)}
          onOpen={openIssue}
        />
      )}
    </div>
  );
}
