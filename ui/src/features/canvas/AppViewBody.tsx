/**
 * CAD-2471: the canvas body of an app view. It holds a reference only; the
 * installed app's own screen is mounted here through the ordinary screen
 * outlet, read-only, under the viewer's own session. A viewer who cannot read
 * the app sees "No access", never another member's data.
 */
import { useCallback, useEffect, useState, type ReactNode } from "react";
import { ApiError } from "../../lib/api";
import { useLocaleTag, useTranslate } from "../../lib/locale";
import { AppGlyph } from "../explorer/shared";
import { InstalledScreenOutlet } from "../workspace-apps/screen/ScreenHost";
import type { ScreenPaneState } from "../workspace-apps/screen/screenFailure";
import type { RoutePatch } from "../workspace-apps/screen/screenRoute";
import { installedAppHref } from "../apps/appViewModel";
import AppLoadingSurface from "../workspace-apps/AppLoadingSurface";
import { readAppScopeFresh, type AppContext, type Installation } from "../workspace-apps/workspaceApps";
import { closeTab, patchAppViewRoute, type CanvasTab } from "./canvasStore";
import type { AppViewRef } from "./appView";
import { setQueryParams } from "../workspace-apps/screen/routeParams";
import { paramChips } from "./viewParams";
import ViewHeader from "./ViewHeader";

type Load =
  | { status: "loading" }
  | { status: "denied" }
  | { status: "failed" }
  | { status: "ready"; installation: Installation; contexts: AppContext[] };

/** A refusal that is the viewer's own lack of access, not a fault. */
export const isNoAccess = (status: number): boolean => status === 401 || status === 403 || status === 404;

function useAppScope(installId: string, attempt: number): Load {
  const [load, setLoad] = useState<Load>({ status: "loading" });
  useEffect(() => {
    const controller = new AbortController();
    setLoad({ status: "loading" });
    readAppScopeFresh(installId, controller.signal).then(
      ([installation, contexts]) => { if (!controller.signal.aborted) setLoad({ status: "ready", installation, contexts }); },
      (error: unknown) => {
        if (controller.signal.aborted) return;
        setLoad(error instanceof ApiError && isNoAccess(error.status) ? { status: "denied" } : { status: "failed" });
      },
    );
    return () => controller.abort();
  }, [installId, attempt]);
  return load;
}

const SECTION_QUERY = /^[a-z][a-z0-9_-]{0,31}$/;

/** A view card's full-page link, from the installation itself: its canonical
 *  `/apps/<key>` path and declared section query, pinned to the install. Null
 *  when the install declares no usable route. */
function installationHref(installation: Installation, app: AppViewRef): string | null {
  const path = installedAppHref(installation.name);
  const sectionQuery = installation.navigation?.section_query;
  if (path === null || sectionQuery === undefined || !SECTION_QUERY.test(sectionQuery)) return null;
  const query = new URLSearchParams({ install: app.installId, ctx: app.contextId, [sectionQuery]: app.route.section });
  if (app.route.record_id !== null) query.set("record", app.route.record_id);
  // The tab's own params travel to the full page as `p.<name>` (CAD-2555).
  setQueryParams(query, app.route.params ?? {});
  return `${path}?${query.toString()}`;
}

/** Tells the tab its frame was refused for lack of access, so the header drops "Open in <App>". */
function FrameDenied({ onDenied, children }: { onDenied: () => void; children: ReactNode }) {
  useEffect(onDenied, [onDenied]);
  return <>{children}</>;
}

export default function AppViewBody({ tab, app, sheet, tabbed, expanded, canExpand, onExpand, onClose, onPaneClose }: {
  tab: CanvasTab;
  app: AppViewRef;
  sheet: boolean;
  /** The tab strip is showing, so it names the view and the header does not. */
  tabbed: boolean;
  expanded: boolean;
  canExpand: boolean;
  onExpand: () => void;
  onClose: () => void;
  /** In a pane (CAD-2635): its close returns the view to a tab. */
  onPaneClose?: () => void;
}) {
  const t = useTranslate();
  const locale = useLocaleTag();
  const [attempt, setAttempt] = useState(0);
  const load = useAppScope(app.installId, attempt);
  // No access (the scope refused, or the frame's own mount did): the tab offers no way into the app.
  const [frameDenied, setFrameDenied] = useState(false);
  const noAccess = load.status === "denied" || frameDenied;
  const appName = load.status === "ready" ? load.installation.title || load.installation.name : app.appLabel || t("App");
  // The frame's own navigation stays on this tab; it never reaches the board URL.
  const onRoute = useCallback((patch: RoutePatch) => patchAppViewRoute(tab.id, patch), [tab.id]);
  const href = app.fromCard === true ? (load.status === "ready" ? installationHref(load.installation, app) : null) : app.href;
  const chips = paramChips(app.route.params, locale);
  const openLabel = t("Open in {app}").replace("{app}", appName);
  const denied = <p className="cv-appview-note" role="alert">{t("No access")}</p>;
  const failure = (state: ScreenPaneState, retry: () => void) =>
    state.kind === "refused" && isNoAccess(state.status) ? <FrameDenied onDenied={() => setFrameDenied(true)}>{denied}</FrameDenied> : (
      <div className="cv-appview-note" role="alert">
        <p>{t("This screen could not be opened.")}</p>
        <button type="button" className="btn btn-ghost" onClick={retry}>{t("Retry")}</button>
      </div>
    );
  return (
    <>
      <ViewHeader
        mark={sheet ? null : <span className="cv-mark"><AppGlyph name={load.status === "ready" ? load.installation.name : app.tag} title={appName} size="xs" /></span>}
        back={onPaneClose ? null : sheet ? { onBack: onClose, expanded: false } : expanded ? { onBack: onExpand, expanded: true } : null}
        title={tabbed && !onPaneClose ? null : tab.title}
        chips={chips}
        note={noAccess ? null : t("Read-only here — open in {app} to edit").replace("{app}", appName)}
        open={noAccess || href === null ? null : {
          label: openLabel,
          href,
          onOpen: sheet ? onClose : () => closeTab(tab.id),
        }}
        expand={sheet ? null : { expanded, enabled: canExpand, onToggle: onExpand }}
        pane={onPaneClose ? { onMoveToTab: onPaneClose } : null}
        onClose={onPaneClose ?? onClose}
      />
      <div className="cv-body cv-appview-body" data-canvas-body data-app-view>
        {load.status === "loading" ? <AppLoadingSurface label={t("Loading…")} variant="canvas" /> : null}
        {load.status === "denied" ? denied : null}
        {load.status === "failed" ? (
          <div className="cv-appview-note" role="alert">
            <p>{t("This screen could not be opened.")}</p>
            <button type="button" className="btn btn-ghost" onClick={() => setAttempt((n) => n + 1)}>{t("Retry")}</button>
          </div>
        ) : null}
        {load.status === "ready" ? (
          <InstalledScreenOutlet
            key={attempt}
            installation={load.installation}
            contexts={load.contexts}
            contextId={app.contextId}
            admitted
            readOnly
            route={app.route}
            onRoute={onRoute}
            renderFailure={failure}
            fallback={denied}
          />
        ) : null}
      </div>
    </>
  );
}
