import { useEffect, useRef, useState } from "react";
import { ApiError } from "../../lib/apiError";
import { useTranslate } from "../../lib/locale";
import Button from "../../ui/Button";
import { notifyAppsChanged, type Installation } from "./workspaceApps";
import { WorkspaceDialog } from "./WorkspaceDialog";
import { applyPrivateApp, checkPrivateApp, type PrivateReview, type PrivateTarget } from "./privateAppUpload";

const ZIP_LIMIT = 8 * 1024 * 1024;

/** The owner reviews a host-checked package before explicitly committing the same File. */
export function PrivateAppDialog({ installation, onClose, onInstalled }: {
  installation?: Installation;
  onClose: () => void;
  onInstalled: () => void;
}) {
  const t = useTranslate();
  const [file, setFile] = useState<File | null>(null);
  const [review, setReview] = useState<PrivateReview | null>(null);
  const [consent, setConsent] = useState(false);
  const [busy, setBusy] = useState<"check" | "apply" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [uncertain, setUncertain] = useState(false);
  const [installed, setInstalled] = useState<Installation | null>(null);
  const reviewHeading = useRef<HTMLHeadingElement>(null);
  const inFlight = useRef(false);
  useEffect(() => { if (review) reviewHeading.current?.focus(); }, [review]);
  const target: PrivateTarget | null = installation ? {
    installId: installation.install_id,
    name: installation.name,
    digest: installation.digest,
    generation: installation.catalog_generation,
  } : null;
  const title = installation ? t("Update private app") : t("Add a private app");
  const close = () => { if (!inFlight.current) onClose(); };
  const selectFile = (selected: File | null) => {
    if (inFlight.current) return;
    setReview(null); setConsent(false); setError(null);
    if (selected && (selected.size === 0 || selected.size > ZIP_LIMIT)) {
      setFile(null);
      setError(t("Choose a non-empty ZIP no larger than 8 MiB."));
      return;
    }
    setFile(selected);
  };
  const check = async () => {
    if (!file || inFlight.current) return;
    inFlight.current = true;
    setBusy("check"); setError(null); setReview(null); setConsent(false);
    try {
      setReview(await checkPrivateApp(file, target));
    } catch (cause) {
      setError(cause instanceof ApiError && cause.status === 404
        ? t("Private app uploads are not available on this workspace yet.")
        : cause instanceof ApiError && cause.status === 403
          ? t("Only a current workspace owner can install private apps.")
          : cause instanceof ApiError && cause.code === "host_incompatible"
            ? t("This package requests features or platform tools this workspace does not support.")
            : cause instanceof ApiError && cause.code === "app_exists"
              ? t("An app with this name already exists in this workspace.")
              : cause instanceof ApiError && cause.code === "installation_stale"
                ? t("The app or package changed. Check it again before installing.")
                : t("The package could not be checked. Check the ZIP and try again."));
    } finally { inFlight.current = false; setBusy(null); }
  };
  const apply = async () => {
    if (!file || !review || !consent || inFlight.current || !review.compatible || review.warnings) return;
    inFlight.current = true;
    setBusy("apply"); setError(null);
    try {
      const result = await applyPrivateApp(file, review);
      setInstalled(result);
      notifyAppsChanged();
      onInstalled();
    } catch (cause) {
      if (cause instanceof ApiError && cause.status >= 400 && cause.status < 500) {
        setReview(null); setConsent(false);
        setError(cause.status === 403
          ? t("Only a current workspace owner can install private apps.")
          : t("The app or package changed. Check it again before installing."));
      } else {
        setReview(null); setUncertain(true); notifyAppsChanged();
      }
    } finally { inFlight.current = false; setBusy(null); }
  };
  return (
    <WorkspaceDialog title={title} onClose={close}>
      {installed ? (
        <div className="space-y-3" role="status">
          <h3 className="text-cardtitle font-semibold text-ink-100">{installation ? t("Private app updated") : t("Private app installed")}</h3>
          <p>{t("The checked version is installed in this workspace.")}</p>
          <p><b>{installed.title}</b> · {installed.version}</p>
          <div className="flex flex-wrap gap-2">
            <Button className="btn-primary" href={`/apps/manage/${encodeURIComponent(installed.install_id)}`}>{t("Manage app")}</Button>
            <Button onClick={close}>{t("Back to apps")}</Button>
          </div>
        </div>
      ) : uncertain ? (
        <div className="space-y-3" role="alert">
          <h3 className="text-cardtitle font-semibold text-ink-100">{t("Installation outcome unknown")}</h3>
          <p>{t("The workspace did not confirm the result. Refresh your apps before trying again.")}</p>
          <Button onClick={close}>{t("Back to apps")}</Button>
        </div>
      ) : (
        <div className="space-y-4">
          <ol className="flex flex-wrap gap-4 text-label text-ink-400" aria-label={t("Installation progress")}>
            <li aria-current={!review ? "step" : undefined}>1 · {t("Choose package")}</li>
            <li aria-current={review ? "step" : undefined}>2 · {t("Review access")}</li>
          </ol>
          {!review ? (
            <>
              <p>{t("Choose a ZIP built with the Cadence app kit. It stays in this workspace, not the public directory.")}</p>
              <label className="block text-label text-ink-200">
                <span className="block mb-2">{t("App package (.zip)")}</span>
                <input type="file" accept=".zip,application/zip" disabled={busy !== null} onChange={(event) => selectFile(event.currentTarget.files?.[0] ?? null)} />
              </label>
              {file && <p className="text-label text-ink-400">{file.name} · {(file.size / 1024).toFixed(0)} KiB</p>}
              {error && <p className="alert fail" role="alert">{error}</p>}
              <div className="flex flex-wrap gap-2">
                <Button className="btn-primary" disabled={!file || busy !== null} onClick={() => void check()}>{busy === "check" ? t("Checking…") : t("Check package")}</Button>
                <Button disabled={busy !== null} onClick={close}>{t("Cancel")}</Button>
              </div>
            </>
          ) : (
            <>
              <h3 ref={reviewHeading} tabIndex={-1} className="text-cardtitle font-semibold text-ink-100">{t("Review your private app")}</h3>
              <p>{t("Review the exact version and requested access before installing.")}</p>
              <dl className="grid gap-1 text-label">
                <div><dt className="inline text-ink-400">{t("App")}: </dt><dd className="inline">{review.title} ({review.name})</dd></div>
                <div><dt className="inline text-ink-400">{t("Version")}: </dt><dd className="inline">{review.version}</dd></div>
                {installation && <div><dt className="inline text-ink-400">{t("Installed version")}: </dt><dd className="inline">{installation.version}</dd></div>}
                <div><dt className="inline text-ink-400">{t("Package digest")}: </dt><dd className="inline"><code className="break-all">{review.digest}</code></dd></div>
                <div><dt className="inline text-ink-400">{t("Visibility")}: </dt><dd className="inline">{t("This workspace only")}</dd></div>
              </dl>
              <div className={review.compatible ? "alert info" : "alert fail"} role="status">{review.compatible ? t("Compatible with this workspace") : t("This package is not compatible with this workspace.")}</div>
              <section aria-label={t("Requested access")} className="space-y-2">
                <h4 className="font-medium text-ink-100">{t("Requested access")}</h4>
                <p className="text-label"><span className="text-ink-400">{t("Declared connections")}:</span> {review.declared.connections.join(", ") || t("None")}</p>
                <div className="text-label">
                  <p className="text-ink-400">{t("Declared capabilities")}</p>
                  {Object.keys(review.declared.capabilities).length === 0 ? <p>{t("None")}</p> :
                    <ul className="list-disc pl-5">{Object.entries(review.declared.capabilities).map(([name, value]) =>
                      <li key={name}><code>{name}</code>: {value.capability} v{value.version} / {value.action} / {value.resource_kind} ({value.effect})</li>)}</ul>}
                </div>
                {installation && <div className="text-label">
                  <p className="text-ink-400">{t("Changes since installed version")}</p>
                  {review.access.connections.added.length === 0 && review.access.connections.removed.length === 0 && Object.keys(review.access.capabilities).length === 0
                    ? <p>{t("No declared access changes.")}</p>
                    : <ul className="list-disc pl-5">
                      {review.access.connections.added.map((name) => <li key={`add-${name}`}>{t("Added connection")}: <code>{name}</code></li>)}
                      {review.access.connections.removed.map((name) => <li key={`remove-${name}`}>{t("Removed connection")}: <code>{name}</code></li>)}
                      {Object.entries(review.access.capabilities).map(([name, change]) => <li key={name}><code>{name}</code>: {change.before && <span>{change.before.capability} v{change.before.version} / {change.before.action} / {change.before.resource_kind} ({change.before.effect}) → </span>}{change.after ? <span>{change.after.capability} v{change.after.version} / {change.after.action} / {change.after.resource_kind} ({change.after.effect})</span> : t("Removed")}</li>)}
                    </ul>}
                </div>}
                {review.previousScreens && <ScreenList label={t("Previous screen access")} entries={review.previousScreens} />}
                <ScreenList label={t("Screen access in this version")} entries={review.screens} />
              </section>
              {review.structural && <section className="text-label"><h4 className="font-medium text-ink-100">{t("Package changes")}</h4>
                {(["added", "changed", "removed"] as const).map((kind) => <p key={kind}>{t(kind === "added" ? "Added" : kind === "changed" ? "Changed" : "Removed")}: {review.structural![kind].join(", ") || t("None")}</p>)}
              </section>}
              {installation && review.digest !== installation.digest && <p className="alert warn">{t("Updating this app revokes its connection and tool grants. Reconnect them after the update.")}</p>}
              <p className="alert warn">{t("This check does not scan for secrets or review app behavior.")}</p>
              {review.warnings && <p className="alert fail" role="alert">{t("The package reported warnings. Resolve them before installing.")}</p>}
              <label className="flex items-start gap-2 text-label"><input type="checkbox" checked={consent} onChange={(event) => setConsent(event.currentTarget.checked)} disabled={busy !== null} /><span>{t("I reviewed this version and its requested access.")}</span></label>
              {error && <p className="alert fail" role="alert">{error}</p>}
              <div className="flex flex-wrap gap-2">
                <Button disabled={busy !== null} onClick={() => { setReview(null); setConsent(false); }}>{t("Back")}</Button>
                <Button className="btn-primary" disabled={!consent || !review.compatible || review.warnings || busy !== null} onClick={() => void apply()}>{busy === "apply" ? t("Installing…") : installation ? t("Install update") : t("Install private app")}</Button>
              </div>
            </>
          )}
        </div>
      )}
    </WorkspaceDialog>
  );
}

function ScreenList({ label, entries }: { label: string; entries: PrivateReview["screens"] }) {
  const t = useTranslate();
  return <div className="text-label"><p className="text-ink-400">{label}</p>{entries.map((screen) => <div key={screen.tag}>
    <p><code>{screen.tag}</code> · {screen.host_contract ?? t("No host contract")}</p>
    <p>{t("Operations")}: {(screen.operations ?? []).join(", ") || t("None")}</p>
    <p>{t("Platform tools")}: {(screen.tools ?? []).join(", ") || t("None")}</p>
    <p>{t("Remote image origins")}: {screen.remote_image_origins.join(", ") || t("None")}</p>
  </div>)}</div>;
}
