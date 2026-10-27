import { useHref } from "../../../lib/useLocation";
import { useLocale } from "../../../lib/locale";
import { Notice } from "../../app-shell/shared/States";
import type { AppContext, Installation } from "../workspaceApps";
import { screenTag } from "./screenProjection";

/**
 * CAD-1519: the one state shown when an installed app's own screen does
 * not mount — no screen declared, not approved or executable, no live
 * context, or this viewer not admitted. Core draws no app UI in its place.
 * `?screen=native` gives the operator the mount facts instead (installation,
 * digest, screen tag and each mount gate), never a second product UI.
 */
export default function ScreenUnavailable({ installation, contexts, contextId, admitted, operator }: {
  installation: Installation; contexts: AppContext[]; contextId: string; admitted: boolean; operator: boolean;
}) {
  const { t } = useLocale();
  const href = useHref();
  const diagnostics = operator && new URLSearchParams(href.split("?")[1] ?? "").get("screen") === "native";
  const message = <Notice state="screen-unavailable">{t("This app's screen is unavailable.")}</Notice>;
  if (!diagnostics) return message;
  const tag = screenTag(installation);
  const liveContext = contexts.some(c => c.id === contextId && c.install_id === installation.install_id && c.state === "active");
  const rows: [string, string][] = [
    ["Installation", `${installation.title || installation.name} (${installation.install_id})`],
    ["Version", installation.version],
    ["Digest", installation.digest],
    ["Screen tag", tag ?? t("none declared")],
    ["Approved", installation.approval.state],
    ["Executable", String(installation.executable)],
    ["Removed", installation.removed == null ? "no" : "yes"],
    ["Context", contextId === "" ? t("none") : `${contextId}${liveContext ? "" : ` (${t("not active")})`}`],
    ["Viewer admitted", String(admitted)],
  ];
  return (
    <section className="card px-4 py-4" aria-label={t("Screen diagnostics")} data-screen-diagnostics>
      {message}
      <dl className="mt-3 grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1 text-label">
        {rows.map(([label, value]) => (
          <div key={label} className="contents">
            <dt className="text-ink-400">{t(label)}</dt>
            <dd className="num text-ink-100 break-all">{value}</dd>
          </div>
        ))}
      </dl>
    </section>
  );
}
