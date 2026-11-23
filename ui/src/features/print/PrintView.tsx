import { useEffect, useState } from "react";
import { ApiError } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import { isNoAccess } from "../canvas/AppViewBody";
import { InstalledScreenOutlet } from "../workspace-apps/screen/ScreenHost";
import { readAppScopeFresh, type AppContext, type Installation } from "../workspace-apps/workspaceApps";
import { parsePrintQuery, printRefusal, printRoute, PRINT_SETTLE_MS } from "./printRequest";
import "./print.css";

declare global {
  interface Window {
    /** Set once the one app view is drawn; the snapshot renderer waits for it. */
    __printReady?: boolean;
  }
}

type Load =
  | { status: "loading" }
  | { status: "refused"; why: "request" | "access" | "screen" | "section" | "failed" }
  | { status: "ready"; installation: Installation; contexts: AppContext[] };

/** The frame did not come up: say so, and keep the page from ever reading as ready. */
function Failed({ onFailed, text }: { onFailed: () => void; text: string }) {
  useEffect(onFailed, [onFailed]);
  return <p className="print-view-note" role="alert">{text}</p>;
}

/**
 * AOS-711 (CAD-2705): one app view, read-only, no chrome, for the snapshot
 * renderer. It mounts the canvas's own screen outlet under the viewer's
 * session, so an owner prints what an owner may see and anyone else gets a
 * refusal. `window.__printReady` is set only when the frame has reported
 * ready (then a short settle for it to paint): never for a refusal.
 */
export default function PrintView({ search, settleMs = PRINT_SETTLE_MS }: { search: string; settleMs?: number }) {
  const t = useTranslate();
  const spec = parsePrintQuery(search);
  const [load, setLoad] = useState<Load>(spec === null ? { status: "refused", why: "request" } : { status: "loading" });
  const [frameFailed, setFrameFailed] = useState(false);
  const installId = spec?.installId ?? null;
  useEffect(() => {
    window.__printReady = false;
    if (spec === null || installId === null) return;
    const controller = new AbortController();
    readAppScopeFresh(installId, controller.signal).then(
      ([installation, contexts]) => {
        if (controller.signal.aborted) return;
        const refusal = printRefusal(spec, installation);
        setLoad(refusal === null ? { status: "ready", installation, contexts } : { status: "refused", why: refusal });
      },
      (error: unknown) => {
        if (!controller.signal.aborted) setLoad({ status: "refused", why: error instanceof ApiError && isNoAccess(error.status) ? "access" : "failed" });
      },
    );
    return () => controller.abort();
    // The request is fixed for the life of the page.
  }, [installId]);

  const [framed, setFramed] = useState(false);
  useEffect(() => {
    if (!framed || frameFailed) return;
    const timer = setTimeout(() => { window.__printReady = true; }, settleMs);
    return () => clearTimeout(timer);
  }, [framed, frameFailed, settleMs]);

  const note = (text: string) => <p className="print-view-note" role="alert">{text}</p>;
  const refused = load.status === "refused" ? load.why : frameFailed ? "failed" : null;
  return (
    <main className="print-view" data-print-view data-print-state={refused ? "refused" : framed ? "ready" : "loading"}>
      {refused === "request" ? note(t("This print request is not valid.")) : null}
      {refused === "access" ? note(t("No access")) : null}
      {refused === "screen" || refused === "section" ? note(t("This view is not available.")) : null}
      {refused === "failed" && load.status === "refused" ? note(t("This screen could not be opened.")) : null}
      {load.status === "ready" && spec !== null ? (
        <div className="print-view-frame">
          <InstalledScreenOutlet
            installation={load.installation}
            contexts={load.contexts}
            contextId="default"
            admitted
            readOnly
            route={printRoute(spec)}
            onNavigation={() => setFramed(true)}
            // A refused or failed mount is never ready: the renderer times out and nothing is shared.
            renderFailure={() => <Failed onFailed={() => setFrameFailed(true)} text={t("This screen could not be opened.")} />}
            fallback={<Failed onFailed={() => setFrameFailed(true)} text={t("No access")} />}
          />
        </div>
      ) : null}
    </main>
  );
}
