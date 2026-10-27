import { useMemo } from "react";
import type { Route } from "../../lib/router";
import { useHref } from "../../lib/useLocation";
import Wiki, { type WikiProps } from "../wiki/Wiki";
import { contextHref, contextRoute, projectWikiRoot } from "./contextRoute";
import Link from "../../ui/Link";
import { useTranslate } from "../../lib/locale";

interface ContextProps {
  project: string;
  readOnly: boolean;
  actor: string;
  onToast: WikiProps["onToast"];
  navHref: (route: Route) => string;
  /** A hosted tenant does not serve the wiki; the project context is wiki-backed. */
  hosted?: boolean;
}

export default function Context({
  project,
  readOnly,
  actor,
  onToast,
  navHref,
  hosted = false,
}: ContextProps) {
  const t = useTranslate();
  const href = useHref();
  const scope = useMemo(() => ({ root: projectWikiRoot(project), label: "Context" }), [project]);
  if (hosted) {
    return (
      <main className="context-workspace" aria-label={`${project} context`}>
        <div className="p-5 text-secondary text-ink-400">{t("The wiki isn’t available on this board.")}</div>
      </main>
    );
  }
  const route = contextRoute(project, href);
  const wikiHref = (next: Route) => next.screen === "wiki" ? contextHref(project, next, href) : navHref(next);
  return (
    <main className="context-workspace" aria-label={`${project} context`}>
      {project === "all" ? <div className="p-5 text-secondary text-ink-400">{t("Choose a project to open its context.")}</div> : route ? (
        <Wiki key={project} scope={scope} route={route} navHref={wikiHref} readOnly={readOnly} actor={actor} onToast={onToast} />
      ) : <div className="p-5" role="alert">{t("This page is outside the project’s context.")} <Link className="lnk" href={`/projects/${encodeURIComponent(project)}/context`}>{t("Open project context")}</Link></div>}
    </main>
  );
}
