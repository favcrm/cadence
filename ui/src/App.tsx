import { useCallback, useEffect, useRef, useState } from "react";
import { api, ApiError, type WriteResp } from "./api";
import Agents from "./components/Agents";
import Board from "./components/Board";
import Drawer from "./components/Drawer";
import Plan from "./components/Plan";
import Sidebar from "./components/Sidebar";
import Toast, { type ToastMsg } from "./components/Toast";
import type { AgentsPayload, Health, IssueCard, IssueDetail, Project } from "./types";

export default function App() {
  const [tab, setTab] = useState<"board" | "plan" | "agents">("board");
  const [project, setProject] = useState("all");
  const [query, setQuery] = useState("");
  const [issues, setIssues] = useState<IssueCard[]>([]);
  const [projects, setProjects] = useState<Project[]>([]);
  const [agents, setAgents] = useState<AgentsPayload | null>(null);
  const [health, setHealth] = useState<Health | null>(null);
  const [openId, setOpenId] = useState<string | null>(null);
  const [openDetail, setOpenDetail] = useState<IssueDetail | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  const [toast, setToast] = useState<ToastMsg | null>(null);
  const toastTimer = useRef<number>(0);

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
    if (openId) {
      api
        .issue(openId)
        .then(setOpenDetail)
        .catch(() => {});
    }
  }, [openId]);

  useEffect(refresh, [refresh]);

  // Live updates: /api/stream pushes `issues|agents|jobs` event names —
  // each one just triggers the normal refresh. EventSource reconnects
  // on its own; the 30 s poll below stays as the fallback while the
  // stream is down.
  useEffect(() => {
    const es = new EventSource("/api/stream");
    es.addEventListener("issues", refresh);
    es.addEventListener("agents", refresh);
    es.addEventListener("jobs", refresh);
    return () => es.close();
  }, [refresh]);

  // Fallback poll — re-read on focus and every 30s while the tab is
  // visible, covering any gap while the stream reconnects.
  useEffect(() => {
    const onFocus = () => refresh();
    const tick = () => {
      if (document.visibilityState === "visible") refresh();
    };
    const timer = setInterval(tick, 30_000);
    addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", tick);
    return () => {
      clearInterval(timer);
      removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", tick);
    };
  }, [refresh]);

  const say = useCallback((kind: ToastMsg["kind"], text: string) => {
    setToast({ kind, text });
    clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(null), 4200);
  }, []);

  /// A write response is authoritative: merge the fresh card into the
  /// board and the fresh detail into the drawer — no second fetch.
  const applyWrite = useCallback(
    (resp: WriteResp, verb: string) => {
      setIssues((prev) => {
        const i = prev.findIndex((c) => c.id === resp.card.id);
        if (i === -1) return [...prev, resp.card];
        const next = prev.slice();
        next[i] = resp.card;
        return next;
      });
      setOpenDetail((d) => (d && d.id === resp.issue.id ? resp.issue : d));
      const warns = (resp.warnings ?? []).filter(Boolean);
      if (warns.length) {
        say("warn", `${verb} · ${warns.join(" · ")}`);
      } else {
        say("ok", verb);
      }
    },
    [say],
  );

  const writeError = useCallback(
    (e: unknown, verb: string) => {
      const err = e as ApiError;
      // A conflict carries the fresh card — resync so the board shows
      // the state that won.
      if (err.card) {
        setIssues((prev) => {
          const i = prev.findIndex((c) => c.id === err.card!.id);
          if (i === -1) return prev;
          const next = prev.slice();
          next[i] = err.card!;
          return next;
        });
      }
      say("err", `${verb} failed: ${err.message ?? e}`);
    },
    [say],
  );

  /// Drag or drawer status change. `rev` is the card's if_rev token;
  /// `rollback` restores the previous card on failure.
  const moveIssue = useCallback(
    (issue: IssueCard, status: string) => {
      if (issue.status === status) return;
      const prev = issue;
      // Optimistic: the card moves now, the write decides for real.
      setIssues((all) =>
        all.map((c) => (c.id === issue.id ? { ...c, status } : c)),
      );
      api
        .patch(issue.id, { status }, issue.rev)
        .then((resp) => applyWrite(resp, `${issue.id} → ${status}`))
        .catch((e) => {
          setIssues((all) => all.map((c) => (c.id === issue.id ? prev : c)));
          writeError(e, `${issue.id} move`);
        });
    },
    [applyWrite, writeError],
  );

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
              onClick={() => setTab("agents")}
              className="chip bg-ink-800 text-ink-200"
            >
              agents
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
              className="chip bg-accent/10 text-accent hover:bg-accent/20 transition-colors"
              title="re-read the folders — writes also land here from the API"
            >
              refresh
            </button>
            <span
              className="hidden sm:inline-flex chip bg-ink-800 text-ink-400"
              title="writes commit to the tracker as operator (ui)"
            >
              writes: operator
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

        {tab === "board" && (
          <Board
            issues={issues}
            projects={projects}
            agents={agents}
            health={health}
            project={project}
            query={query}
            onQuery={setQuery}
            onOpen={openIssue}
            onMove={moveIssue}
            onCreated={applyWrite}
            onError={writeError}
            onAgents={() => setTab("agents")}
          />
        )}
        {tab === "agents" && <Agents payload={agents} onOpenIssue={openIssue} />}
        {tab === "plan" && <Plan />}
      </div>

      {openId && (
        <Drawer
          key={openId}
          id={openId}
          agents={agents}
          projects={projects}
          pmDir={health?.pm_dir}
          detail={openDetail}
          onClose={() => setOpenId(null)}
          onOpen={openIssue}
          onWrite={applyWrite}
          onError={writeError}
        />
      )}

      <Toast msg={toast} />
    </div>
  );
}
