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
  const references = (
    <details className="context-references">
      <summary className="text-secondary font-medium text-ink-400">Repository references</summary>
      <p className="text-label text-ink-500 mt-2 mb-4">Reviewed repository documents. Changes follow the repository’s review process.</p>
      <ProjectContext project={project} context={context} loading={contextLoading} error={contextError} onRetry={onRetryContext} />
    </details>
  );
  return (
    <main className="context-workspace" aria-label={`${project} context`}>
      {project === "all" ? <div className="p-5 text-secondary text-ink-400">Choose a project to open its context.</div> : route ? (
        <Wiki key={project} scope={scope} route={route} navHref={wikiHref} readOnly={readOnly} actor={actor} onToast={onToast} paneFooter={route.mode === "browse" ? references : undefined} />
      ) : <div className="p-5" role="alert">This page is outside the project’s context. <Link className="lnk" href={`/projects/${encodeURIComponent(project)}/context`}>Open project context</Link></div>}
    </main>
  );
}
