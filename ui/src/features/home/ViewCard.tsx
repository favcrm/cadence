import { createContext, useContext, useEffect, useState } from "react";
import { ApiError } from "../../lib/api";
import { useLocaleTag, useTranslate } from "../../lib/locale";
import { TurnCard, type CardDensity } from "../../ui/TurnCard";
import { appViewId, sameRoute } from "../canvas/appView";
import { openAppView, useCanvas } from "../canvas/canvasStore";
import { paramsLine } from "../canvas/viewParams";
import { workspaceApps } from "../workspace-apps/workspaceApps";
import { HOME_VIEW_SCOPE, parseViewRef, plainText, viewCardAppView, viewHere, type ViewScope } from "./chatCards";
import type { ThreadEntry } from "./thread";

/**
 * CAD-2475: the host's `view_ref` card (agenticos-v2 AOS-654). A reference to
 * one screen of an installed app, never its content: the button opens the
 * CAD-2471 canvas app tab (`openAppView`), which mounts the screen under the
 * viewer's own session. A payload that does not parse, or names a section the
 * installation does not declare, shows the entry's text line instead.
 *
 * States: Open (its tab is showing its record), Opened (its tab moved away; the
 * Open button brings it back), the Open button (closed), Shown in <App> (CAD-2512:
 * the card is for the app page already on screen, so clicking moves that page and
 * opens no tab), No access (the install cannot be read: 401/403) and Not found
 * (404, removed, or a reference the tab would refuse).
 */

/** Which install and context the surrounding conversation belongs to (the app pane sets it). */
export const ViewScopeContext = createContext<ViewScope>(HOME_VIEW_SCOPE);

type Probe = { status: "checking" | "ok" | "denied" | "missing"; sections: string[] | null };

/** Reads the install once per card: its availability and the sections it declares. */
function useInstallProbe(installId: string | null): Probe {
  const [probe, setProbe] = useState<Probe>({ status: "checking", sections: null });
  useEffect(() => {
    if (installId === null) return;
    const controller = new AbortController();
    setProbe({ status: "checking", sections: null });
    workspaceApps.detail(installId, controller.signal).then(
      (installation) => {
        if (controller.signal.aborted) return;
        if (installation.removed != null || installation.state === "removed") setProbe({ status: "missing", sections: null });
        else setProbe({ status: "ok", sections: installation.navigation ? installation.navigation.sections.map((s) => s.id) : null });
      },
      (error: unknown) => {
        if (controller.signal.aborted) return;
        const status = error instanceof ApiError ? error.status : 0;
        // Any other failure leaves the button: opening the tab reports it itself.
        setProbe({ status: status === 404 ? "missing" : status === 401 || status === 403 ? "denied" : "ok", sections: null });
      },
    );
    return () => controller.abort();
  }, [installId]);
  return probe;
}

export type ViewPlace = "showing" | "moved" | "closed" | "here";
export type ViewState = ViewPlace | "denied" | "missing";

/** The card's state: a refused install beats everything; an unopenable reference is Not found. */
export function viewState(probe: Probe["status"], openable: boolean, place: ViewPlace): ViewState {
  if (probe === "denied") return "denied";
  if (probe === "missing" || !openable) return "missing";
  return place;
}

export function ViewCard({ entry, density = "full" }: { entry: ThreadEntry; density?: CardDensity }) {
  const t = useTranslate();
  const locale = useLocaleTag();
  const scope = useContext(ViewScopeContext);
  const canvas = useCanvas();
  const model = parseViewRef(entry);
  const probe = useInstallProbe(model?.installId ?? null);
  const line = <p className="text-micro text-ink-500 break-words chatcard-line" data-kind="view_ref">· {plainText(entry.text, 500)}</p>;
  if (!model) return line;
  // The host writes only declared sections; a screen that does not list it means the card is stale.
  if (probe.sections !== null && !probe.sections.includes(model.section)) return line;
  const ref = viewCardAppView(model, scope);
  const tab = ref === null ? undefined : canvas.tabs.find((x) => x.id === appViewId(ref));
  const moved = ref !== null && tab?.app !== undefined && !sameRoute(tab.app.route, ref.route);
  const where: ViewPlace = viewHere(model, scope) ? "here" : moved ? "moved" : ref !== null && canvas.open && canvas.active === appViewId(ref) ? "showing" : "closed";
  const state = viewState(probe.status, ref !== null, where);
  const status = {
    showing: { text: t("Open"), tone: "ok" as const },
    moved: { text: t("Opened"), tone: "muted" as const },
    here: undefined,
    denied: { text: t("No access"), tone: "warn" as const },
    missing: { text: t("Not found"), tone: "muted" as const },
    closed: undefined,
  }[state];
  // A declared view names its place by its label (CAD-2555) and shows its params on a line of their own.
  const place = model.viewLabel ?? (model.recordId === null ? model.section : `${model.section} / ${model.recordId}`);
  const params = paramsLine(model.params, locale);
  return (
    <TurnCard
      kind="view"
      className="card chatcard viewcard"
      bodyClassName="chatcard-body"
      density={density}
      data-kind="view_ref"
      data-view-state={state}
      label={t("Canvas view card")}
      icon={<span className="viewcard-initial" aria-hidden>{(Array.from(model.title)[0] ?? "?").toUpperCase()}</span>}
      kindLabel={t("View")}
      subject={<span className="viewcard-title" title={model.title}>{model.title}</span>}
      status={status}
      footer={
        ref === null ? null : state === "here" ? (
          <button type="button" className="btn btn-ghost" data-view-here onClick={() => scope.showHere?.(ref.route)}>
            {t("Shown in {app}").replace("{app}", model.appLabel)}
          </button>
        ) : state === "closed" || state === "moved" ? (
          <button type="button" className="btn btn-primary" data-view-open onClick={() => openAppView(ref)}>
            {t("Open")}
          </button>
        ) : null
      }
    >
      <p className="text-label text-ink-300 viewcard-place" title={`${model.appLabel} · ${place}`}>
        {model.appLabel} · {place}
      </p>
      {params === "" ? null : <p className="text-label text-ink-300 viewcard-params" data-view-params title={params}>{params}</p>}
    </TurnCard>
  );
}
