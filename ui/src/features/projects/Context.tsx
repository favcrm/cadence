import { useEffect, useMemo, useState } from "react";
import type { Route } from "../../lib/router";
import { useHref } from "../../lib/useLocation";
import Wiki, { type WikiProps } from "../wiki/Wiki";
import { contextHref, contextRoute, projectWikiRoot } from "./contextRoute";
import Workflows from "./Workflows";
import type { Viewer } from "./work";
import Link from "../../ui/Link";
import { useTranslate } from "../../lib/locale";

interface ContextProps {
  project: string;
  readOnly: boolean;
  /** Why writes are blocked, when the board knows it (App's `writeBlock`). */
  readOnlyReason?: string | null;
  actor: string;
  onToast: WikiProps["onToast"];
  navHref: (route: Route) => string;
  /** The board does not serve the wiki (see wikiAvailable); the project context is wiki-backed. */
  noWiki?: boolean;
  /** A hosted board (undefined until the board's meta is known). */
  hosted?: boolean;
  viewerSub?: string | null;
  workflowsEnabled?: boolean;
  initialSubtab?: "knowledge" | "workflows";
  workflowsProps?: {
    viewer: Viewer;
    onOpenIssue: (id: string) => void;
    onHome: () => void;
  };
}

export default function Context({
  project,
  readOnly,
  readOnlyReason,
  actor,
  onToast,
  navHref,
  noWiki = false,
  hosted,
  viewerSub,
  workflowsEnabled = false,
  initialSubtab = "knowledge",
  workflowsProps,
}: ContextProps) {
  const t = useTranslate();
  const href = useHref();
  const hasWorkflows = workflowsEnabled && Boolean(workflowsProps) && project !== "all";
  const [subtab, setSubtab] = useState<"knowledge" | "workflows">(() =>
    noWiki && hasWorkflows ? "workflows" : initialSubtab,
  );
  useEffect(() => {
    setSubtab(noWiki && hasWorkflows ? "workflows" : initialSubtab);
  }, [project, initialSubtab, noWiki, hasWorkflows]);
  const scope = useMemo(() => ({ root: projectWikiRoot(project), label: "Context" }), [project]);
  if (noWiki && !hasWorkflows) {
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
      {hasWorkflows && (
        <div className="context-subnav" role="group" aria-label={t("Context sections")}>
          {!noWiki && (
            <button
              type="button"
              aria-pressed={subtab === "knowledge"}
              onClick={() => setSubtab("knowledge")}
              className={`btn btn-sm ${subtab === "knowledge" ? "btn-primary" : "btn-ghost"}`}
            >
              {t("Knowledge & Briefs")}
            </button>
          )}
          <button
            type="button"
            aria-pressed={subtab === "workflows"}
            onClick={() => setSubtab("workflows")}
            className={`btn btn-sm ${subtab === "workflows" ? "btn-primary" : "btn-ghost"}`}
          >
            {t("Workflows")}
          </button>
        </div>
      )}
      {subtab === "workflows" && hasWorkflows && workflowsProps ? (
        <Workflows
          embedded
          project={project}
          viewer={workflowsProps.viewer}
          onOpenIssue={workflowsProps.onOpenIssue}
          onHome={workflowsProps.onHome}
        />
      ) : project === "all" ? (
        <div className="p-5 text-secondary text-ink-400">{t("Choose a project to open its context.")}</div>
      ) : route ? (
        <Wiki key={project} hosted={hosted} viewerSub={viewerSub} scope={scope} route={route} navHref={wikiHref} readOnly={readOnly} readOnlyReason={readOnlyReason} actor={actor} onToast={onToast} />
      ) : (
        <div className="p-5" role="alert">{t("This page is outside the project’s context.")} <Link className="lnk" href={`/projects/${encodeURIComponent(project)}/context`}>{t("Open project context")}</Link></div>
      )}
    </main>
  );
}
