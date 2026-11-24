import { useTranslate } from "../../../lib/locale";
import Button from "../../../ui/Button";
import { screenFailurePane, type ScreenPaneState } from "./screenFailure";

export default function ScreenFailurePane({ failure, appName, onRetry, onReinstall, busy }: {
  failure: ScreenPaneState;
  appName: string;
  onRetry: () => void;
  onReinstall?: () => void;
  busy?: boolean;
}) {
  const t = useTranslate();
  const pane = screenFailurePane(failure, onReinstall !== undefined);
  const title = failure.kind === "needs_host_update"
    ? t(pane.title).replace("{required}", () => failure.required).replace("{current}", () => failure.current)
    : t(pane.title).replace("{app}", () => appName);
  return (
    <section className="card px-4 py-5" aria-label={t("Installed app screen")} data-screen-pane={failure.kind} role="status">
      <p className="wa-muted">{title}</p>
      <div className="wa-row">
        {pane.actions.includes("sign_in") && <Button href="/login">{t("Sign in")}</Button>}
        {pane.actions.includes("retry") && <Button onClick={onRetry}>{t("Retry")}</Button>}
        {pane.actions.includes("reinstall") && onReinstall && <Button onClick={onReinstall} loading={busy}>{t("Reinstall")}</Button>}
        {pane.actions.includes("check_update") && <Button onClick={() => window.location.reload()}>{t("Check for update")}</Button>}
      </div>
    </section>
  );
}
