import type { Project, ProjectContext } from "../../lib/types";

const DOC_STATE_CHIP: Record<string, string> = {
  ready: "bg-ok/15 text-ok",
  current: "bg-ok/15 text-ok",
  stale: "bg-warn/10 text-warn",
  dirty: "bg-warn/10 text-warn",
  uncompared: "bg-warn/10 text-warn",
};

/// Compact reading of a selected project's declared scope and tracked
/// document manifest from `/api/projects/:key/context`.
export default function ProjectScope({
  project,
  projects,
  context,
  contextLoading,
  onOpenContext,
}: {
  project: string;
  projects: Project[];
  context: ProjectContext | null;
  contextLoading: boolean;
  onOpenContext: () => void;
}) {
  const meta = projects.find((p) => p.key === project);
  const docs = context?.documents.filter((d) => d.selected) ?? [];
  const shown = docs.slice(0, 6);
  return (
    <section>
      <div className="slabel mb-2">project context</div>
      <div className="card px-4 py-3.5 space-y-3">
        {meta && (
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5 text-micro">
            {meta.repos.map((repo, i) => (
              <span key={i} className="num text-ink-300">
                {repo.remote ?? repo.path}
              </span>
            ))}
            {meta.components.map((component) => (
              <span
                key={component}
                className="chip bg-ink-800 text-ink-400 !py-[.15rem]"
              >
                {component}
              </span>
            ))}
            {meta.default_owner && (
              <span className="text-ink-500">
                default owner {meta.default_owner}
              </span>
            )}
          </div>
        )}
        {contextLoading && !context && (
          <p className="text-label text-ink-500">
            reading the project's tracked manifest…
          </p>
        )}
        {docs.length > 0 && (
          <div className="divide-y divide-ink-700/60">
            {shown.map((doc) => (
              <div
                key={doc.id}
                className="py-1.5 flex flex-wrap items-baseline gap-x-2 gap-y-0.5"
              >
                <span className="text-label text-ink-200">{doc.title}</span>
                <span className="num text-micro text-ink-500">{doc.path}</span>
                {doc.required && <span className="kicker">required</span>}
                {!DOC_STATE_CHIP[doc.state] && (
                  <span className="chip bg-fail/10 text-fail !py-[.15rem]">
                    {doc.state}
                  </span>
                )}
              </div>
            ))}
          </div>
        )}
        {context && docs.length > shown.length && (
          <p className="text-micro text-ink-500">
            Showing {shown.length} of {docs.length} source documents
          </p>
        )}
        <button
          onClick={onOpenContext}
          className="lnk text-label"
        >
          Open project context →
        </button>
      </div>
    </section>
  );
}
