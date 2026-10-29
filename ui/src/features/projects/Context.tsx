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
  /** The board does not serve the wiki (see wikiAvailable); the project context is wiki-backed. */
  noWiki?: boolean;
  /** A hosted board (undefined until the board's meta is known). */
  hosted?: boolean;
  viewerSub?: string | null;
}

export default function Context({
  project,
  readOnly,
  actor,
  onToast,
  navHref,
  noWiki = false,
  hosted,
  viewerSub,
}: ContextProps) {
  const t = useTranslate();
  const href = useHref();
  const scope = useMemo(() => ({ root: projectWikiRoot(project), label: "Context" }), [project]);
  if (noWiki) {
    return (
      <main className="context-workspace" aria-label={`${project} context`}>
        <div className="p-5 text-secondary text-ink-400">
          {t("The wiki isn’t available on this board.")} <Link className="lnk" href={navHref({ screen: "home" })}>{t("Back to Home")}</Link>
        </div>
      </main>
    );
  }
  const route = contextRoute(project, href);
  const wikiHref = (next: Route) => next.screen === "wiki" ? contextHref(project, next, href) : navHref(next);
  return (
    <main className="context-workspace" aria-label={`${project} context`}>
      {project === "all" ? <div className="p-5 text-secondary text-ink-400">{t("Choose a project to open its context.")}</div> : route ? (
        <Wiki key={project} hosted={hosted} viewerSub={viewerSub} scope={scope} route={route} navHref={wikiHref} readOnly={readOnly} actor={actor} onToast={onToast} />
      ) : <div className="p-5" role="alert">{t("This page is outside the project’s context.")} <Link className="lnk" href={`/projects/${encodeURIComponent(project)}/context`}>{t("Open project context")}</Link></div>}
    </main>
  );
}
