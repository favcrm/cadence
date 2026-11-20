import { useEffect, useId, useRef, useState } from "react";
import { ApiError } from "../../lib/apiError";
import { onPrivateReadsPaused, onPrivateSessionReset, privateReadsAllowed, privateSessionGeneration } from "../../lib/privateSession";
import { useTranslate } from "../../lib/locale";
import Button from "../../ui/Button";
import { AppGlyph } from "../explorer/shared";
import { installedAppHref } from "../apps/appViewModel";
import { notifyAppsChanged, type Installation } from "./workspaceApps";
import { WorkspaceDialog } from "./WorkspaceDialog";
import { applyPrivateApp, checkPrivateApp, decideHostedPrivateReview, fetchGithubPrivateApp, getHostedPrivateReview, githubPrivateAppAvailable, type HostedPrivateReview, type PrivateReview, type PrivateTarget } from "./privateAppUpload";

const ZIP_LIMIT = 8 * 1024 * 1024;
const STALE_REVIEW = "Workspace apps changed after this review was created. Ask the requester for a new review.";

/** The owner reviews a host-checked package before explicitly committing the same File. */
export function PrivateAppDialog({ installation, reviewId, onClose, onInstalled }: {
  installation?: Installation;
  reviewId?: string;
  onClose: () => void;
  onInstalled: () => void;
}) {
  const t = useTranslate();
  const githubInputId = useId();
  const fileInputId = useId();
  const [file, setFile] = useState<File | null>(null);
  const [dragging, setDragging] = useState(false);
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
        const stale = request.state === "pending" && request.catalogStale;
        setError(stale ? STALE_REVIEW : null);
        setReview(request.state === "pending" && !stale ? request.review : null);
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
  const subtitle = reviewId !== undefined
    ? t("Inspect the requested package and its workspace access before approving.")
    : t("Private apps stay in this workspace and never appear in the public directory.");
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
        (decision === "install" && (hostedReview.catalogStale || !consent || !hostedReview.review.compatible || hostedReview.review.warnings))) return;
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
        setError(STALE_REVIEW);
      } else if (cause instanceof ApiError && cause.status === 403) {
        setError("Only a current workspace owner can review this request.");
      } else {
        setUncertain(true);
        if (decision === "install") notifyAppsChanged();
      }
    } finally { inFlight.current = false; setBusy(null); }
  };

  const installedHref = installed ? installedAppHref(installed.name) : null;

  const footer = hostedLoading ? null : reviewId !== undefined && !review && !uncertain ? (
    <div className="pa-foot-row">
      <span />
      <Button onClick={close}>{t("Back to apps")}</Button>
    </div>
  ) : installed ? (
    <div className="pa-foot-row">
      <Button className="btn-ghost" onClick={close}>{t("Back to apps")}</Button>
      <div className="flex flex-wrap gap-2">
        <Button href={`/apps/manage/${encodeURIComponent(installed.install_id)}`}>{t("Manage app")}</Button>
        {installedHref && <Button className="btn-primary" href={installedHref}>{t("Open app")}</Button>}
      </div>
    </div>
  ) : uncertain ? (
    <div className="pa-foot-row">
      <span />
      <Button onClick={close}>{t("Back to apps")}</Button>
    </div>
  ) : !review ? (
    <div className="pa-foot-row">
      <Button className="btn-ghost" disabled={busy !== null} onClick={close}>{t("Cancel")}</Button>
      <Button
        className="btn-primary"
        disabled={(source === "upload" ? !file : !githubUrl.trim()) || busy !== null}
        onClick={() => void check()}
      >
        {busy === "check" ? t("Checking…") : t("Check package")}
      </Button>
    </div>
  ) : (
    <>
      <label className="pa-consent-row">
        <input type="checkbox" checked={consent} onChange={(event) => setConsent(event.currentTarget.checked)} disabled={busy !== null} />
        <span>{t("I reviewed this version and its requested access.")}</span>
      </label>
      <div className="pa-foot-row">
        {hostedReview ? (
          <>
            <Button disabled={busy !== null} onClick={() => void decide("reject")}>
              {busy === "reject" ? t("Rejecting…") : t("Reject request")}
            </Button>
            <Button
              className="btn-primary"
              disabled={!consent || !review.compatible || review.warnings || busy !== null}
              onClick={() => void decide("install")}
            >
              {busy === "apply" ? t("Installing…") : t("Approve and install")}
            </Button>
          </>
        ) : (
          <>
            <Button disabled={busy !== null} onClick={() => { setReview(null); setCheckedFile(null); setProvenance(null); setConsent(false); }}>
              {t("Back")}
            </Button>
            <Button
              className="btn-primary"
              disabled={!consent || !review.compatible || review.warnings || busy !== null}
              onClick={() => void apply()}
            >
              {busy === "apply" ? t("Installing…") : installation ? t("Install update") : t("Install private app")}
            </Button>
          </>
        )}
      </div>
    </>
  );

  return (
    <WorkspaceDialog
      title={title}
      subtitle={subtitle}
      badge={t("This workspace only")}
      footer={footer}
      className="wa-private-dialog"
      onClose={close}
    >
      {hostedLoading ? (
        <p className="text-label text-ink-400" role="status">{t("Loading review…")}</p>
      ) : reviewId !== undefined && !review && !uncertain ? (
        <div className="space-y-3" role={error ? "alert" : "status"}>
          <p className="text-secondary text-ink-200">{error ? t(error) : hostedReview?.state === "installed" ? t("This request has been installed.") : hostedReview?.state === "rejected" ? t("This request was rejected.") : hostedReview?.state === "expired" ? t("This request has expired.") : t("This review is missing or unavailable.")}</p>
        </div>
      ) : installed ? (
        <div className="pa-installed" role="status">
          <div className="pa-installed-badge" aria-hidden>✓</div>
          <h3 className="text-cardtitle font-semibold text-ink-100">{installation ? t("Private app updated") : t("Private app installed")}</h3>
          <p className="text-label text-ink-400">{t("The checked version is installed in this workspace.")}</p>
          <div className="pa-installed-pkg">
            <AppGlyph name={installed.name} title={installed.title} size="sm" />
            <div className="text-left min-w-0">
              <div className="text-secondary font-semibold text-ink-100 truncate">{installed.title}</div>
              <div className="text-micro text-ink-400 num">v{installed.version}</div>
            </div>
          </div>
        </div>
      ) : uncertain ? (
        <div className="space-y-2" role="alert">
          <h3 className="text-cardtitle font-semibold text-ink-100">{t(reviewId !== undefined ? "Decision outcome unknown" : "Installation outcome unknown")}</h3>
          <p className="text-label text-ink-400">{t(reviewId !== undefined ? "The workspace did not confirm the decision. Open the review link again to see its current status." : "The workspace did not confirm the result. Refresh your apps before trying again.")}</p>
        </div>
      ) : (
        <div className="pa-body-stack">
          {reviewId === undefined && (
            <ol className="pa-stepper" aria-label={t("Installation progress")}>
              <li className="pa-step" aria-current={!review ? "step" : undefined} data-done={review ? "true" : undefined}>
                <span className="pa-step-num" aria-hidden>{review ? "✓" : "1"}</span>
                <span>{t("Choose package")}</span>
              </li>
              <li className="pa-step-sep" aria-hidden />
              <li className="pa-step" aria-current={review ? "step" : undefined}>
                <span className="pa-step-num" aria-hidden>2</span>
                <span>{t("Review access")}</span>
              </li>
            </ol>
          )}
          {!review ? (
            <>
              {githubAvailable && (
                <fieldset disabled={busy !== null} className="pa-source-fieldset">
                  <legend className="pa-legend">{t("Package source")}</legend>
                  <div className="pa-source-grid">
                    <label className={`pa-source-card ${source === "upload" ? "is-selected" : ""}`}>
                      <input
                        className="sr-only"
                        type="radio"
                        name="private-app-source"
                        checked={source === "upload"}
                        onChange={() => { setSource("upload"); setError(null); }}
                      />
                      <span className="pa-source-ic" aria-hidden>
                        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                          <path d="M12 16V4m0 0-4 4m4-4 4 4M4 17v2a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-2" />
                        </svg>
                      </span>
                      <span className="min-w-0">
                        <span className="pa-source-title">{t("Upload a ZIP")}</span>
                        <span className="pa-source-desc">{t("Local Cadence app kit bundle, up to 8 MiB.")}</span>
                      </span>
                    </label>
                    <label className={`pa-source-card ${source === "github" ? "is-selected" : ""}`}>
                      <input
                        className="sr-only"
                        type="radio"
                        name="private-app-source"
                        checked={source === "github"}
                        onChange={() => { setSource("github"); setError(null); }}
                      />
                      <span className="pa-source-ic" aria-hidden>
                        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                          <path d="M9 19c-4.5 1.4-4.5-2.5-6-3m12 6v-3.8c0-1 .1-1.5-.5-2.1 2.4-.3 5-1.2 5-5.4 0-1.2-.4-2.2-1.1-3 .1-.3.5-1.4-.1-2.9 0 0-.9-.3-3 1.1a10.4 10.4 0 0 0-5.4 0C6.8 4.4 5.9 4.7 5.9 4.7c-.6 1.5-.2 2.6-.1 2.9-.7.8-1.1 1.8-1.1 3 0 4.2 2.6 5.1 5 5.4-.4.4-.6 1-.6 2V22" />
                        </svg>
                      </span>
                      <span className="min-w-0">
                        <span className="pa-source-title">{t("From GitHub")}</span>
                        <span className="pa-source-desc">{t("Tagged public release .zip with SHA-256 checksum.")}</span>
                      </span>
                    </label>
                  </div>
                </fieldset>
              )}
              {source === "upload" ? (
                <div className="space-y-2">
                  <input
                    id={fileInputId}
                    key={epoch.current}
                    className="sr-only"
                    type="file"
                    accept=".zip,application/zip"
                    disabled={busy !== null}
                    onChange={(event) => selectFile(event.currentTarget.files?.[0] ?? null)}
                  />
                  {file ? (
                    <div className="pa-file-pill">
                      <AppGlyph name={file.name} title={file.name} size="sm" />
                      <div className="min-w-0 flex-1">
                        <div className="text-secondary font-medium text-ink-100 truncate">{file.name}</div>
                        <div className="text-micro text-ink-400 num">{Math.max(1, Math.round(file.size / 1024))} KiB · .zip</div>
                      </div>
                      <Button className="btn-sm btn-ghost" disabled={busy !== null} onClick={() => selectFile(null)}>
                        {t("Remove")}
                      </Button>
                    </div>
                  ) : (
                    <label
                      htmlFor={fileInputId}
                      className={`pa-dropzone ${dragging ? "is-drag" : ""}`}
                      onDragOver={(event) => { event.preventDefault(); if (busy === null) setDragging(true); }}
                      onDragLeave={() => setDragging(false)}
                      onDrop={(event) => {
                        event.preventDefault();
                        setDragging(false);
                        if (busy !== null) return;
                        selectFile(event.dataTransfer.files?.[0] ?? null);
                      }}
                    >
                      <span className="pa-drop-ic" aria-hidden>
                        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                          <path d="M12 15V3m0 0-4 4m4-4 4 4M4 17v2a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-2" />
                        </svg>
                      </span>
                      <span className="pa-drop-title">{t("Drop your .zip package here, or click to browse")}</span>
                      <span className="pa-drop-sub">{t("App package (.zip)")} · {t("Max 8 MiB")}</span>
                    </label>
                  )}
                </div>
              ) : (
                <div className="text-label text-ink-200 space-y-2">
                  <label className="block font-medium" htmlFor={githubInputId}>{t("Public GitHub release ZIP URL")}</label>
                  <input
                    id={githubInputId}
                    aria-describedby={`${githubInputId}-help`}
                    className="field w-full num"
                    type="url"
                    maxLength={512}
                    value={githubUrl}
                    disabled={busy !== null}
                    onChange={(event) => { setGithubUrl(event.currentTarget.value); setError(null); }}
                    placeholder="https://github.com/owner/repo/releases/download/tag/app.zip"
                  />
                  <p id={`${githubInputId}-help`} className="text-micro text-ink-400 leading-relaxed">
                    {t("Copy the app ZIP link under Releases. GitHub’s source code ZIP is not an app package.")}
                  </p>
                </div>
              )}
              {error && <p className="alert fail" role="alert">{error}</p>}
            </>
          ) : (
            <>
              <h3 ref={reviewHeading} tabIndex={-1} className="sr-only">{t("Review your private app")}</h3>
              <div className="pa-pkg-head">
                <AppGlyph name={review.name} title={review.title} size="sm" />
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-2">
                    <p className="text-cardtitle font-semibold text-ink-100 break-words">{review.title}</p>
                    <span className="chip num">
                      {installation ? `${installation.version} → ${review.version}` : `v${review.version}`}
                    </span>
                    <span className={`chip ${review.compatible ? "ok" : "fail"}`} role="status">
                      {review.compatible ? t("Compatible") : t("Incompatible")}
                    </span>
                  </div>
                  {review.summary && <p className="text-label text-ink-400 mt-0.5 wrap-anywhere">{review.summary}</p>}
                  {hostedReview && (
                    <p className="text-micro text-ink-500 mt-1">
                      {t("Expires")}: {new Date(hostedReview.expiresAt).toLocaleString()}
                    </p>
                  )}
                </div>
              </div>

              {!review.compatible && (
                <div className="alert fail" role="alert">{t("This package is not compatible with this workspace.")}</div>
              )}

              <section aria-label={t("Requested access")} className="pa-section">
                <h4 className="pa-legend">{t("Requested access")}</h4>
                <div className="pa-perm-card">
                  <div className="pa-perm-row">
                    <span className="pa-perm-k">{t("Connections")}</span>
                    <span className="pa-perm-v break-words">
                      {review.declared.connections.length
                        ? review.declared.connections.join(", ")
                        : <span className="text-ink-400">{t("No named connections requested.")}</span>}
                    </span>
                  </div>
                  <div className="pa-perm-row">
                    <span className="pa-perm-k">{t("Capabilities")}</span>
                    <div className="pa-perm-v">
                      {Object.keys(review.declared.capabilities).length === 0 ? (
                        <span className="text-ink-400">{t("No separate access grants requested.")}</span>
                      ) : (
                        <ul className="list-disc pl-4 space-y-1">
                          {Object.entries(review.declared.capabilities).map(([name, value]) => (
                            <li key={name} className="break-words">
                              {describeCapability(value, t)} <span className="text-ink-400 num">({name})</span>
                            </li>
                          ))}
                        </ul>
                      )}
                    </div>
                  </div>
                  {review.screens.length > 0 && (
                    <div className="pa-perm-row">
                      <span className="pa-perm-k">{t("Screens")}</span>
                      <div className="pa-perm-v space-y-1.5">
                        {review.screens.map((screen) => (
                          <div key={screen.tag} className="space-y-0.5">
                            <p>{t("Screen operations")}: {screen.operations === null ? t("Not declared") : screen.operations.length === 0 ? t("None") : null}</p>
                            {screen.operations && screen.operations.length > 0 && (
                              <ul className="list-disc break-words pl-4">
                                {screen.operations.map((operation) => <li key={operation}>{screenOperationLabel(operation, t)}</li>)}
                              </ul>
                            )}
                            <p className="break-words text-ink-400">{t("Screen tools")}: {screen.tools === null ? t("Not declared") : screen.tools.join(", ") || t("None")}</p>
                          </div>
                        ))}
                      </div>
                    </div>
                  )}
                  {review.screens.some((screen) => screen.remote_image_origins.length > 0) && (
                    <div className="pa-perm-row">
                      <span className="pa-perm-k">{t("Loads images from")}</span>
                      <span className="pa-perm-v break-all">
                        {review.screens.flatMap((screen) => screen.remote_image_origins).join(", ")}
                      </span>
                    </div>
                  )}
                  {(installation || review.target) && (
                    <div className="pa-perm-row">
                      <span className="pa-perm-k">{t("Changes since installed version")}</span>
                      <div className="pa-perm-v">
                        {review.access.connections.added.length === 0 && review.access.connections.removed.length === 0 && Object.keys(review.access.capabilities).length === 0 ? (
                          <span className="text-ink-400">{t("No declared access changes.")}</span>
                        ) : (
                          <ul className="list-disc pl-4 space-y-1">
                            {review.access.connections.added.map((name) => <li key={`add-${name}`}>{t("Added connection")}: <code>{name}</code></li>)}
                            {review.access.connections.removed.map((name) => <li key={`remove-${name}`}>{t("Removed connection")}: <code>{name}</code></li>)}
                            {Object.entries(review.access.capabilities).map(([name, change]) => (
                              <li key={name} className="break-words">
                                {name}: {change.before && <>{describeCapability(change.before, t)} → </>}
                                {change.after ? describeCapability(change.after, t) : t("Removed")}
                              </li>
                            ))}
                          </ul>
                        )}
                      </div>
                    </div>
                  )}
                </div>
                {review.previousScreens && JSON.stringify(review.previousScreens) !== JSON.stringify(review.screens) && (
                  <p className="text-label text-warn">{t("Screen access changed. Review the details below.")}</p>
                )}
              </section>

              <details className="pa-tech-details">
                <summary>{t("Technical details")}</summary>
                <div className="mt-3 space-y-2 break-words text-micro text-ink-300">
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

              {((installation && review.digest !== installation.digest) || (review.target && review.digest !== review.target.digest)) && (
                <p className="alert warn">{t("Updating this app revokes its connection and tool grants. Reconnect them after the update.")}</p>
              )}
              <p className="alert warn">{t("This check does not scan for secrets or review app behavior.")}</p>
              {review.warnings && <p className="alert fail" role="alert">{t("The package reported warnings. Resolve them before installing.")}</p>}
              {error && <p className="alert fail" role="alert">{error}</p>}
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
