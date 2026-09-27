import ProjectContext from "./ProjectContext";
import type { ProjectContext as ProjectContextPayload } from "../../lib/types";
import { useMemo } from "react";
import type { Route } from "../../lib/router";
import { useHref } from "../../lib/useLocation";
import Wiki, { type WikiProps } from "../wiki/Wiki";
import { contextHref, contextRoute, projectWikiRoot } from "./contextRoute";
import Link from "../../ui/Link";

interface ContextProps {
  project: string;
  context: ProjectContextPayload | null;
  contextLoading: boolean;
  contextError: string | null;
  onRetryContext: () => void;
  readOnly: boolean;
  actor: string;
  onToast: WikiProps["onToast"];
  navHref: (route: Route) => string;
}

export default function Context({
  project,
  context,
  contextLoading,
  contextError,
  onRetryContext,
  readOnly,
  actor,
  onToast,
  navHref,
}: ContextProps) {
  const href = useHref();
  const scope = useMemo(() => ({ root: projectWikiRoot(project), label: "Context" }), [project]);
  const route = contextRoute(project, href);
  const wikiHref = (next: Route) => next.screen === "wiki" ? contextHref(project, next, href) : navHref(next);
  return (
    <main className="px-4 lg:px-8 pt-6 pb-9 w-full">
      <div className="mb-5">
        <h1 className="text-section font-semibold text-ink-100">Context</h1>
        <p className="text-secondary text-ink-400 mt-1">Project brief, research, decisions, and shared knowledge.</p>
      </div>
      {project === "all" ? <div className="card project-card-padding text-secondary text-ink-400">Choose a project to open its context.</div> : route ? (
        <Wiki key={project} scope={scope} route={route} navHref={wikiHref} readOnly={readOnly} actor={actor} onToast={onToast} />
      ) : <div className="card project-card-padding" role="alert">This page is outside the project’s context. <Link className="lnk" href={`/projects/${encodeURIComponent(project)}/context`}>Open project context</Link></div>}
      {project !== "all" && <details className="context-references mt-5">
        <summary className="text-secondary font-medium text-ink-300">Repository references</summary>
        <p className="text-label text-ink-500 mt-2 mb-4">Reviewed documents from the project repository. Edit these through the repository’s review process.</p>
        <ProjectContext project={project} context={context} loading={contextLoading} error={contextError} onRetry={onRetryContext} />
      </details>}
    </main>
  );
}
