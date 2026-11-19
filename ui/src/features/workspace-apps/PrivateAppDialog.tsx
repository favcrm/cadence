import { useEffect, useId, useRef, useState } from "react";
import { ApiError } from "../../lib/apiError";
import { onPrivateReadsPaused, onPrivateSessionReset, privateReadsAllowed, privateSessionGeneration } from "../../lib/privateSession";
import { useTranslate } from "../../lib/locale";
import Button from "../../ui/Button";
import { AppGlyph } from "../explorer/shared";
import { notifyAppsChanged, type Installation } from "./workspaceApps";
import { WorkspaceDialog } from "./WorkspaceDialog";
import { applyPrivateApp, checkPrivateApp, decideHostedPrivateReview, fetchGithubPrivateApp, getHostedPrivateReview, githubPrivateAppAvailable, type HostedPrivateReview, type PrivateReview, type PrivateTarget } from "./privateAppUpload";

const ZIP_LIMIT = 8 * 1024 * 1024;

/** The owner reviews a host-checked package before explicitly committing the same File. */
export function PrivateAppDialog({ installation, reviewId, onClose, onInstalled }: {
  installation?: Installation;
  reviewId?: string;
  onClose: () => void;
  onInstalled: () => void;
}) {
  const t = useTranslate();
  const githubInputId = useId();
  const [file, setFile] = useState<File | null>(null);
  const [source, setSource] = useState<"upload" | "github">("upload");
  const [githubAvailable, setGithubAvailable] = useState(false);
  const [githubUrl, setGithubUrl] = useState("");
  const [checkedFile, setCheckedFile] = useState<File | null>(null);
  const [provenance, setProvenance] = useState<{ url: string; digest: string } | null>(null);
  const [review, setReview] = useState<PrivateReview | null>(null);
  const [consent, setConsent] = useState(false);
  const [hostedReview, setHostedReview] = useState<HostedPrivateReview | null>(null);
  const [hostedLoading, setHostedLoading] = useState(Boolean(reviewId !== undefined));
  const [busy, setBusy] = useState<"check" | "apply" | "reject" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [uncertain, setUncertain] = useState(false);
  const [installed, setInstalled] = useState<Installation | null>(null);
  const reviewHeading = useRef<HTMLHeadingElement>(null);
  const inFlight = useRef(false);
  const epoch = useRef(0);
  useEffect(() => {
    const controller = new AbortController();
    const current = epoch.current;
    if (reviewId === undefined) void githubPrivateAppAvailable(controller.signal).then((available) => {
      if (!controller.signal.aborted && epoch.current === current && privateReadsAllowed()) setGithubAvailable(available);
    });
    else {
      setHostedLoading(true);
      setError(null);
      void getHostedPrivateReview(reviewId, controller.signal).then((request) => {
        if (controller.signal.aborted || epoch.current !== current || !privateReadsAllowed()) return;
        setHostedReview(request);
        setReview(request.state === "pending" ? request.review : null);
        setUncertain(false);
      }).catch(() => {
        if (!controller.signal.aborted && epoch.current === current) setError("This review is missing or unavailable.");
      }).finally(() => { if (!controller.signal.aborted && epoch.current === current) setHostedLoading(false); });
    }
    const invalidate = () => {
      epoch.current++;
      setGithubAvailable(false); setSource("upload"); setReview(null); setCheckedFile(null);
      setProvenance(null); setConsent(false); setGithubUrl(""); setFile(null); setError(null);
      setHostedReview(null); setHostedLoading(false);
      if (reviewId !== undefined) setError("This review is missing or unavailable.");
    };
    const stopPause = onPrivateReadsPaused(invalidate);
    const stopReset = onPrivateSessionReset(invalidate);
    return () => { epoch.current++; controller.abort(); stopPause(); stopReset(); };
  }, [reviewId]);
  useEffect(() => { if (review) reviewHeading.current?.focus(); }, [review]);
  const target: PrivateTarget | null = installation ? {
    installId: installation.install_id,
    name: installation.name,
    digest: installation.digest,
    generation: installation.catalog_generation,
  } : null;
  const title = reviewId !== undefined ? t("Review private app request") : installation ? t("Update private app") : t("Add a private app");
  const close = () => { if (!inFlight.current) onClose(); };
  const selectFile = (selected: File | null) => {
    if (inFlight.current) return;
    setReview(null); setCheckedFile(null); setProvenance(null); setConsent(false); setError(null);
    if (selected && (selected.size === 0 || selected.size > ZIP_LIMIT)) {
      setFile(null);
      setError(t("Choose a non-empty ZIP no larger than 8 MiB."));
      return;
    }
    setFile(selected);
  };
  const check = async () => {
    if ((source === "upload" ? !file : !githubUrl.trim()) || inFlight.current) return;
    const current = epoch.current;
    const generation = privateSessionGeneration();
    inFlight.current = true;
    setBusy("check"); setError(null); setReview(null); setCheckedFile(null); setProvenance(null); setConsent(false);
    try {
      const fetched = source === "github" ? await fetchGithubPrivateApp(githubUrl.trim()) : null;
      if (epoch.current !== current || !privateReadsAllowed() || privateSessionGeneration() !== generation ||
          (fetched && fetched.sessionGeneration !== generation)) return;
      const selected = fetched?.file ?? file;
      if (!selected) return;
      const checked = await checkPrivateApp(selected, target);
      if (epoch.current !== current || !privateReadsAllowed() || privateSessionGeneration() !== generation) return;
      setCheckedFile(selected);
      setProvenance(fetched ? { url: githubUrl.trim(), digest: fetched.digest } : null);
      setReview(checked);
    } catch (cause) {
      if (epoch.current !== current || !privateReadsAllowed() || privateSessionGeneration() !== generation) return;
      setError(t(packageCheckError(cause, source)));
    } finally { inFlight.current = false; setBusy(null); }
  };
  const apply = async () => {
    if (!checkedFile || !review || !consent || inFlight.current || !review.compatible || review.warnings) return;
    const current = epoch.current;
    inFlight.current = true;
    setBusy("apply"); setError(null);
    try {
      const result = await applyPrivateApp(checkedFile, review);
      if (epoch.current !== current || !privateReadsAllowed()) { setUncertain(true); notifyAppsChanged(); return; }
      setInstalled(result);
      notifyAppsChanged();
      onInstalled();
    } catch {
      // An intermediary can reject the response after the install was forwarded.
      // Refresh the workspace before attempting another install or update.
      setReview(null); setUncertain(true); notifyAppsChanged();
    } finally { inFlight.current = false; setBusy(null); }
  };
  const decide = async (decision: "install" | "reject") => {
    if (!hostedReview || hostedReview.state !== "pending" || inFlight.current ||
        (decision === "install" && (!consent || !hostedReview.review.compatible || hostedReview.review.warnings))) return;
    const current = epoch.current;
    inFlight.current = true;
    setBusy(decision === "install" ? "apply" : "reject"); setError(null);
    try {
      const result = await decideHostedPrivateReview(hostedReview, decision);
      if (epoch.current !== current || !privateReadsAllowed()) return;
      setHostedReview(result); setReview(null);
      if (result.state === "installed") { notifyAppsChanged(); onInstalled(); }
    } catch (cause) {
      if (epoch.current !== current || !privateReadsAllowed()) return;
      setReview(null);
      if (cause instanceof ApiError && cause.status === 409 && (cause.code === "installation_stale" || cause.code === "app_exists" || cause.code === "package_changed")) {
        setError("This review no longer matches the workspace. Ask for a new review before installing.");
      } else if (cause instanceof ApiError && cause.status === 403) {
        setError("Only a current workspace owner can review this request.");
      } else {
        setUncertain(true);
        if (decision === "install") notifyAppsChanged();
      }
    } finally { inFlight.current = false; setBusy(null); }
  };
  return (
    <WorkspaceDialog title={title} onClose={close}>
      {hostedLoading ? (
        <p role="status">{t("Loading review…")}</p>
      ) : reviewId !== undefined && !review && !uncertain ? (
        <div className="space-y-3" role={error ? "alert" : "status"}>
          <p>{error ? t(error) : hostedReview?.state === "installed" ? t("This request has been installed.") : hostedReview?.state === "rejected" ? t("This request was rejected.") : hostedReview?.state === "expired" ? t("This request has expired.") : t("This review is missing or unavailable.")}</p>
          <Button onClick={close}>{t("Back to apps")}</Button>
        </div>
      ) : installed ? (
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
          <h3 className="text-cardtitle font-semibold text-ink-100">{t(reviewId !== undefined ? "Decision outcome unknown" : "Installation outcome unknown")}</h3>
          <p>{t(reviewId !== undefined ? "The workspace did not confirm the decision. Open the review link again to see its current status." : "The workspace did not confirm the result. Refresh your apps before trying again.")}</p>
          <Button onClick={close}>{t("Back to apps")}</Button>
        </div>
      ) : (
        <div className="space-y-4">
          {reviewId === undefined && <ol className="flex flex-wrap gap-4 text-label text-ink-400" aria-label={t("Installation progress")}>
            <li aria-current={!review ? "step" : undefined}>1 · {t("Choose package")}</li>
            <li aria-current={review ? "step" : undefined}>2 · {t("Review access")}</li>
          </ol>}
          {!review ? (
            <>
              <p>{t("Choose a ZIP built with the Cadence app kit. It stays in this workspace, not the public directory.")}</p>
              {githubAvailable && (
                <fieldset disabled={busy !== null} className="space-y-2 text-label">
                  <legend className="mb-2 text-ink-200">{t("Package source")}</legend>
                  <label className="flex items-center gap-2"><input type="radio" name="private-app-source" checked={source === "upload"} onChange={() => { setSource("upload"); setError(null); }} />{t("Upload a ZIP")}</label>
                  <label className="flex items-center gap-2"><input type="radio" name="private-app-source" checked={source === "github"} onChange={() => { setSource("github"); setError(null); }} />{t("From GitHub")}</label>
                </fieldset>
              )}
              {source === "upload" ? (
                <>
                  <label className="block text-label text-ink-200">
                    <span className="block mb-2">{t("App package (.zip)")}</span>
                    <input key={epoch.current} type="file" accept=".zip,application/zip" disabled={busy !== null} onChange={(event) => selectFile(event.currentTarget.files?.[0] ?? null)} />
                  </label>
                  {file && <p className="text-label text-ink-400">{file.name} · {(file.size / 1024).toFixed(0)} KiB</p>}
                </>
              ) : (
                <div className="text-label text-ink-200">
                  <label className="block mb-2" htmlFor={githubInputId}>{t("Public GitHub release ZIP URL")}</label>
                  <input id={githubInputId} aria-describedby={`${githubInputId}-help`} className="field w-full" type="url" maxLength={512} value={githubUrl} disabled={busy !== null} onChange={(event) => { setGithubUrl(event.currentTarget.value); setError(null); }} placeholder="https://github.com/owner/repo/releases/download/tag/app.zip" />
                  <p id={`${githubInputId}-help`} className="mt-2 text-ink-400">{t("Copy the app ZIP link under Releases. GitHub’s source code ZIP is not an app package.")}</p>
                </div>
              )}
              {error && <p className="alert fail" role="alert">{error}</p>}
              <div className="flex flex-wrap gap-2">
                <Button className="btn-primary" disabled={(source === "upload" ? !file : !githubUrl.trim()) || busy !== null} onClick={() => void check()}>{busy === "check" ? t("Checking…") : t("Check package")}</Button>
                <Button disabled={busy !== null} onClick={close}>{t("Cancel")}</Button>
              </div>
            </>
          ) : (
            <>
              <h3 ref={reviewHeading} tabIndex={-1} className="text-cardtitle font-semibold text-ink-100">{t("Review your private app")}</h3>
              <div className="flex items-start gap-3 rounded-lg border border-ink-700 bg-ink-875 p-3">
                <AppGlyph name={review.title} size="sm" />
                <div className="min-w-0">
                  <p className="text-cardtitle font-semibold text-ink-100 break-words">{review.title}</p>
                  {review.summary && <p className="text-label wrap-anywhere">{review.summary}</p>}
                  <p className="text-label text-ink-400 break-words">
                    {installation ? `${installation.version} → ${review.version}` : `${t("Version")} ${review.version}`} · {t("This workspace only")}
                  </p>
                </div>
              </div>
              {hostedReview && <p className="text-label text-ink-400">
                {t("Expires")}: {new Date(hostedReview.expiresAt).toLocaleString()}
              </p>}
              <div className={review.compatible ? "alert info" : "alert fail"} role="status">{review.compatible ? t("Compatible with this workspace") : t("This package is not compatible with this workspace.")}</div>
              <section aria-label={t("Requested access")} className="space-y-2">
                <h4 className="font-medium text-ink-100">{t("Requested access")}</h4>
                <p className="text-label break-words">{review.declared.connections.length
                  ? <>{t("Accounts this app asks to connect")}: {review.declared.connections.join(", ")}</>
                  : t("No named connections requested.")}</p>
                {Object.keys(review.declared.capabilities).length === 0
                  ? <p className="text-label">{t("No separate access grants requested.")}</p> : (
                  <ul className="list-disc pl-5 text-label">
                    {Object.entries(review.declared.capabilities).map(([name, value]) =>
                      <li key={name} className="break-words">{describeCapability(value, t)} <span className="text-ink-400">({name})</span></li>)}
                  </ul>
                )}
                {review.screens.map((screen) => (
                  <div key={screen.tag} className="space-y-1 text-label">
                    <p>{t("Screen operations")}: {screen.operations === null ? t("Not declared") : screen.operations.length === 0 ? t("None") : null}</p>
                    {screen.operations && screen.operations.length > 0 && (
                      <ul className="list-disc break-words pl-5">
                        {screen.operations.map((operation) => <li key={operation}>{screenOperationLabel(operation, t)}</li>)}
                      </ul>
                    )}
                    <p className="break-words">{t("Screen tools")}: {screen.tools === null ? t("Not declared") : screen.tools.join(", ") || t("None")}</p>
                  </div>
                ))}
                {review.screens.some((screen) => screen.remote_image_origins.length > 0) && (
                  <p className="text-label break-all">{t("Loads images from")}: {review.screens.flatMap((screen) => screen.remote_image_origins).join(", ")}</p>
                )}
                {(installation || review.target) && <div className="text-label">
                  <p className="text-ink-400">{t("Changes since installed version")}</p>
                  {review.access.connections.added.length === 0 && review.access.connections.removed.length === 0 && Object.keys(review.access.capabilities).length === 0
                    ? <p>{t("No declared access changes.")}</p>
                    : <ul className="list-disc pl-5">
                      {review.access.connections.added.map((name) => <li key={`add-${name}`}>{t("Added connection")}: <code>{name}</code></li>)}
                      {review.access.connections.removed.map((name) => <li key={`remove-${name}`}>{t("Removed connection")}: <code>{name}</code></li>)}
                      {Object.entries(review.access.capabilities).map(([name, change]) => (
                        <li key={name} className="break-words">
                          {name}: {change.before && <>{describeCapability(change.before, t)} → </>}
                          {change.after ? describeCapability(change.after, t) : t("Removed")}
                        </li>
                      ))}
                    </ul>}
                </div>}
                {review.previousScreens && JSON.stringify(review.previousScreens) !== JSON.stringify(review.screens) && (
                  <p className="text-label">{t("Screen access changed. Review the details below.")}</p>
                )}
              </section>
              <details className="rounded-lg border border-ink-700 p-3 text-label">
                <summary className="cursor-pointer font-medium text-ink-100">{t("Technical details")}</summary>
                <div className="mt-3 space-y-2 break-words">
                  <p>{t("App")}: <code>{review.name}</code></p>
                  <p>{t("Package digest")}: <code className="break-all">{review.digest}</code></p>
                  {(provenance || hostedReview) && <>
                    <p>{t("GitHub release URL")}: <code className="break-all">{hostedReview?.source ?? provenance?.url}</code></p>
                    <p>{t("Downloaded ZIP SHA-256")}: <code className="break-all">{hostedReview?.zipSha256 ?? provenance?.digest}</code></p>
                  </>}
                  {Object.entries(review.declared.capabilities).map(([name, value]) =>
                    <p key={name}><code>{name}</code>: {formatCapability(value)}</p>)}
                  {Object.entries(review.access.capabilities).map(([name, { before, after }]) => (
                    <p key={`change-${name}`}><code>{name}</code>: {before && <>{formatCapability(before)} → </>}{after ? formatCapability(after) : t("Removed")}</p>
                  ))}
                  {review.previousScreens && <ScreenList label={t("Previous screen access")} entries={review.previousScreens} />}
                  <ScreenList label={t("Screen access in this version")} entries={review.screens} />
                  {review.structural
                    ? <section><h4>{t("Package changes")}</h4>{(["added", "changed", "removed"] as const).map((kind) =>
                        <p key={kind}>{t(kind === "added" ? "Added" : kind === "changed" ? "Changed" : "Removed")}: {review.structural?.[kind].join(", ") || t("None")}</p>)}</section>
                    : <p>{t("Package files")}: {review.files.join(", ")}</p>}
                </div>
              </details>
              {((installation && review.digest !== installation.digest) || (review.target && review.digest !== review.target.digest)) && <p className="alert warn">{t("Updating this app revokes its connection and tool grants. Reconnect them after the update.")}</p>}
              <p className="alert warn">{t("This check does not scan for secrets or review app behavior.")}</p>
              {review.warnings && <p className="alert fail" role="alert">{t("The package reported warnings. Resolve them before installing.")}</p>}
              <label className="flex items-start gap-2 text-label"><input type="checkbox" checked={consent} onChange={(event) => setConsent(event.currentTarget.checked)} disabled={busy !== null} /><span>{t("I reviewed this version and its requested access.")}</span></label>
              {error && <p className="alert fail" role="alert">{error}</p>}
              <div className="flex flex-wrap gap-2">
                {hostedReview ? <>
                  <Button disabled={busy !== null} onClick={() => void decide("reject")}>{busy === "reject" ? t("Rejecting…") : t("Reject request")}</Button>
                  <Button className="btn-primary" disabled={!consent || !review.compatible || review.warnings || busy !== null} onClick={() => void decide("install")}>{busy === "apply" ? t("Installing…") : t("Approve and install")}</Button>
                </> : <>
                  <Button disabled={busy !== null} onClick={() => { setReview(null); setCheckedFile(null); setProvenance(null); setConsent(false); }}>{t("Back")}</Button>
                  <Button className="btn-primary" disabled={!consent || !review.compatible || review.warnings || busy !== null} onClick={() => void apply()}>{busy === "apply" ? t("Installing…") : installation ? t("Install update") : t("Install private app")}</Button>
                </>}
              </div>
            </>
          )}
        </div>
      )}
    </WorkspaceDialog>
  );
}

function packageCheckError(cause: unknown, source: "upload" | "github"): string {
  if (cause instanceof ApiError) {
    if (cause.status === 403) return "Only a current workspace owner can install private apps.";
    if (source === "upload" && cause.status === 404) return "Private app uploads are not available on this workspace yet.";
    switch (cause.code) {
      case "host_incompatible": return "This package requests features or platform tools this workspace does not support.";
      case "app_exists": return "An app with this name already exists in this workspace.";
      case "installation_stale": return "The app or package changed. Check it again before installing.";
      case "github_release_url":
      case "github_asset_not_found": return "Use a ZIP link from a tagged public GitHub release.";
      case "github_asset_unpinned": return "This release has no ZIP checksum. Upload the ZIP instead.";
      case "github_asset_changed": return "The ZIP no longer matches its release checksum. Check the release again.";
      case "payload_too_large": return "Choose a non-empty ZIP no larger than 8 MiB.";
    }
  }
  return source === "github"
    ? "The GitHub package could not be fetched or checked. Check the release URL and try again."
    : "The package could not be checked. Check the ZIP and try again.";
}

type Capability = PrivateReview["declared"]["capabilities"][string];

function screenOperationLabel(operation: string, t: (english: string) => string): string {
  switch (operation) {
    case "calendar.query": return t("Read workspace calendar items, including task and routine titles");
    case "routines.list": return t("Read workspace routines");
    case "catalog.call": return t("Run a granted platform tool with app data");
    case "smtp.test-send": return t("Send a test email through the configured account");
    default: return operation;
  }
}

function describeCapability(value: Capability, t: (english: string) => string): string {
  if (value.capability === "file.upload") return t("Attach files to this app");
  if (value.effect === "send") return t("Publish through a connected account");
  if (value.capability === "catalog.tool" && value.action === "invoke")
    return t(value.effect === "read" ? "Read with an approved tool" : "Prepare drafts with an approved tool");
  const effect = t(value.effect === "read" ? "Read from a connected account" : "Prepare drafts with a connected account");
  return `${effect} · ${value.capability.replace(/[.-]/g, " ")} / ${value.action.replace(/_/g, " ")}`;
}

function formatCapability(value: Capability): string {
  return `${value.capability} v${value.version} / ${value.action} / ${value.resource_kind} (${value.effect})`;
}

function ScreenList({ label, entries }: { label: string; entries: PrivateReview["screens"] }) {
  const t = useTranslate();
  return <div className="text-label"><p className="text-ink-400">{label}</p>{entries.map((screen) => <div key={screen.tag}>
    <p><code>{screen.tag}</code> · {screen.host_contract ?? t("No host contract")}</p>
    <p>{t("Operations")}: {screen.operations === null ? t("Not declared") : screen.operations.join(", ") || t("None")}</p>
    <p>{t("Platform tools")}: {screen.tools === null ? t("Not declared") : screen.tools.join(", ") || t("None")}</p>
    <p>{t("Remote image origins")}: {screen.remote_image_origins.join(", ") || t("None")}</p>
  </div>)}</div>;
}
