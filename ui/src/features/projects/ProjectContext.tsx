import type { ProjectContext as ProjectContextPayload } from "../../lib/types";

interface ProjectContextProps {
  project: string;
  context: ProjectContextPayload | null;
  loading: boolean;
  error: string | null;
  onRetry: () => void;
}

function stateTone(state: string): string {
  if (state === "ready" || state === "current") return "text-ok bg-ok/10";
  if (state === "stale" || state === "dirty" || state === "uncompared") {
    return "text-warn bg-warn/10";
  }
  return "text-fail bg-fail/10";
}

function StateChip({ label, state }: { label: string; state: string }) {
  return <span className={`chip ${stateTone(state)}`}>{label}: {state}</span>;
}

function shortRevision(revision?: string): string {
  return revision ? revision.slice(0, 12) : "unavailable";
}

export default function ProjectContext({
  project,
  context,
  loading,
  error,
  onRetry,
}: ProjectContextProps) {
  if (project === "all") {
    return (
      <section className="card border-info/30 p-5">
        <div className="slabel">selected project context</div>
        <h2 className="text-section font-semibold text-ink-100 mt-2">Choose a project</h2>
        <p className="text-secondary text-ink-400 mt-2">
          Start here reads the fixed tracked manifest for one selected project.
          It does not read a repository until a project is selected.
        </p>
      </section>
    );
  }

  if (loading && !context) {
    return (
      <section className="card border-info/30 p-5" aria-live="polite">
        <div className="slabel">selected project context · {project}</div>
        <h2 className="text-section font-semibold text-ink-100 mt-2">Loading Start here</h2>
        <p className="text-secondary text-ink-400 mt-2">Pinning the repository HEAD and reading tracked documents.</p>
      </section>
    );
  }

  if (error && !context) {
    return (
      <section className="card border-fail/40 p-5" aria-live="polite">
        <div className="slabel text-fail">selected project context · {project}</div>
        <h2 className="text-section font-semibold text-ink-100 mt-2">Context unavailable</h2>
        <p className="text-secondary text-ink-400 mt-2">{error}</p>
        <button type="button" className="rounded border border-ink-600 px-3 py-2 text-label text-ink-300 hover:border-accent/60 hover:text-accent transition-colors mt-4" onClick={onRetry}>Retry</button>
      </section>
    );
  }

  if (!context) {
    return (
      <section className="card border-info/30 p-5" aria-live="polite">
        <div className="slabel">selected project context · {project}</div>
        <h2 className="text-section font-semibold text-ink-100 mt-2">Preparing Start here</h2>
        <p className="text-secondary text-ink-400 mt-2">The previous project context has been cleared.</p>
      </section>
    );
  }
  const snapshot = context.snapshot;
  const observed = snapshot.head_revision;
  return (
    <section className="space-y-5" aria-live="polite">
      <div className="flex flex-wrap items-baseline gap-3">
        <div>
          <div className="slabel">Project context</div>
          <h2 className="text-section font-semibold text-ink-100 mt-2">Start here</h2>
          <p className="text-label text-ink-500 mt-2">Project goals, architecture, and guidance in their reviewed reading order.</p>
        </div>
        <span className="kicker">Read-only documents</span>
      </div>

      <div className="card p-5">
        <div className="flex items-baseline justify-between gap-3">
          <div>
            <div className="slabel">document references</div>
            <h3 className="text-ink-100 font-medium mt-1">Reviewed reading order</h3>
          </div>
          <span className="num text-label text-ink-500">{context.documents.length} entries</span>
        </div>
        <div className="divide-y divide-ink-700/80 mt-3">
          {context.documents.map((document) => (
            <div key={document.id} className="py-3 first:pt-0 last:pb-0">
              <div className="flex flex-wrap items-center gap-2">
                <span className="num text-ink-100">{document.id}</span>
                <StateChip label="state" state={document.state} />
                {document.required && <span className="kicker">required</span>}
              </div>
              <div className="text-label text-ink-400 mt-1">{document.title} · <span className="num">{document.path}</span></div>
              <div className="text-label text-ink-500 mt-1">{document.selection_reason}</div>
              {document.reason && <div className="text-label text-fail mt-1">{document.reason}</div>}
              {document.excerpt && (
                <details className="mt-2">
                  <summary className="text-label text-accent cursor-pointer">Read excerpt{document.truncated ? " · truncated" : ""}</summary>
                  <pre className="num text-label text-ink-300 leading-relaxed whitespace-pre-wrap mt-2 max-h-64 overflow-auto">{document.excerpt}</pre>
                </details>
              )}
            </div>
          ))}
        </div>
      </div>

      <details className="card p-5"><summary className="cursor-pointer text-secondary text-ink-300">Source and retrieval details · {context.state}</summary><div className="mt-4 space-y-4">      <div className="card p-5">
        <div className="flex flex-wrap items-center gap-2">
          <StateChip label="bundle" state={context.state} />
          <StateChip label="revision" state={snapshot.revision_state} />
          {snapshot.dirty === true && <StateChip label="worktree" state="dirty" />}
          {snapshot.dirty === null && <StateChip label="worktree" state="unknown" />}
          {snapshot.dirty_truncated && <span className="chip text-warn bg-warn/10">dirty listing bounded</span>}
          <StateChip label="manifest" state={context.manifest.state} />
        </div>
        <div className="grid sm:grid-cols-2 gap-4 mt-5 text-secondary">
          <div>
            <div className="slabel">observed HEAD</div>
            <div className="num text-ink-100 mt-1 break-all">{shortRevision(observed)}</div>
            {snapshot.expected_revision && snapshot.revision_state === "stale" && (
              <div className="text-label text-warn mt-1">expected {shortRevision(snapshot.expected_revision)}</div>
            )}
          </div>
          <div>
            <div className="slabel">declared source</div>
            <div className="num text-ink-300 mt-1 break-all">{snapshot.repo_identity ?? "unavailable"}</div>
            <div className="text-label text-ink-500 mt-1">HEAD bytes stay pinned even when the worktree is dirty.</div>
          </div>
        </div>
        {snapshot.error && <p className="text-label text-warn mt-4">{snapshot.error}</p>}
      </div>

      <div className="card p-5">
        <div className="flex items-baseline justify-between gap-3">
          <div>
            <div className="slabel">manifest</div>
            <h3 className="text-ink-100 font-medium mt-1">{context.manifest.path}</h3>
          </div>
          {loading && <span className="kicker">refreshing</span>}
        </div>
        {context.manifest.entries_omitted ? (
          <p className="text-label text-warn mt-3">
            {context.manifest.entries_omitted} manifest entries omitted at the bounded entry limit.
          </p>
        ) : null}
        {context.manifest.errors.length > 0 && (
          <ul className="mt-3 space-y-1 text-label text-fail">
            {context.manifest.errors.map((message, index) => <li key={`${index}-${message}`}>{message}</li>)}
          </ul>
        )}
      </div>

</div></details>
      <details className="card p-5"><summary className="text-secondary text-ink-300 cursor-pointer">Verified lessons · {context.memories.matched_total} matches</summary><div className="mt-4">
        <div className="flex items-baseline justify-between gap-3">
          <div>
            <div className="slabel">verified memory context</div>
            <h3 className="text-ink-100 font-medium mt-1">CAD-191 retrieval</h3>
          </div>
          <span className="kicker">{context.memories.matched_total} eligible matches</span>
        </div>
        {context.memories.lessons ? (
          <pre className="num text-label text-ink-300 leading-relaxed whitespace-pre-wrap mt-3 max-h-64 overflow-auto">{context.memories.lessons}</pre>
        ) : (
          <p className="text-secondary text-ink-500 mt-3">No eligible lesson was selected for this context.</p>
        )}
        {context.memories.withheld_total > 0 && (
          <p className="text-label text-warn mt-3">
            {context.memories.withheld_total} memory records withheld; showing {context.memories.withheld.length} reasons
            {context.memories.withheld_omitted > 0 ? ` (${context.memories.withheld_omitted} omitted by bound)` : ""}.
          </p>
        )}
        {context.memories.withheld.length > 0 && (
          <ul className="mt-2 space-y-1 text-label text-ink-500">
            {context.memories.withheld.map((memory) => <li key={memory.id}><span className="num text-ink-300">{memory.id}</span>: {memory.reason}</li>)}
          </ul>
        )}
        {context.memories.load_errors_total > 0 && (
          <div className="mt-3">
            <p className="text-label text-fail">
              {context.memories.load_errors_total} project memory load errors surfaced
              {context.memories.load_errors_omitted > 0 ? `; ${context.memories.load_errors_omitted} omitted by bound` : ""}.
            </p>
            {context.memories.load_errors.length > 0 && (
              <ul className="mt-2 space-y-1 text-label text-fail max-h-32 overflow-auto">
                {context.memories.load_errors.map((error, index) => <li key={`${index}-${error}`}>{error}</li>)}
              </ul>
            )}
          </div>
        )}
      </div></details>

      {context.limits.response_truncated && (
        <p className="text-label text-warn">Response excerpts were shortened to remain within the serialized response bound.</p>
      )}
    </section>
  );
}
