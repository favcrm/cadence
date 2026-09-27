import { useMemo } from "react";
import type { Route } from "../../lib/router";
import { useHref } from "../../lib/useLocation";
import Wiki, { type WikiProps } from "../wiki/Wiki";
import { contextHref, contextRoute, projectWikiRoot } from "./contextRoute";
import Link from "../../ui/Link";

interface ContextProps {
  project: string;
  readOnly: boolean;
  actor: string;
  onToast: WikiProps["onToast"];
  navHref: (route: Route) => string;
}

export default function Context({
  project,
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
    <main className="context-workspace" aria-label={`${project} context`}>
      {project === "all" ? <div className="p-5 text-secondary text-ink-400">Choose a project to open its context.</div> : route ? (
        <Wiki key={project} scope={scope} route={route} navHref={wikiHref} readOnly={readOnly} actor={actor} onToast={onToast} />
      ) : <div className="p-5" role="alert">This page is outside the project’s context. <Link className="lnk" href={`/projects/${encodeURIComponent(project)}/context`}>Open project context</Link></div>}
    </main>
  );
}
