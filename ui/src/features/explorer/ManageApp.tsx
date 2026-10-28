import { useEffect, useRef, useState } from "react";
import Link from "../../ui/Link";
import Button from "../../ui/Button";
import {
  appExplorer,
  workspaceApps,
  type HomeInstallation,
  type Installation,
  type RemovePreview,
  type UpgradeProposal,
  notifyAppsChanged,
} from "../workspace-apps/workspaceApps";
import type { Viewer } from "../projects/work";
import { useInstallations } from "../workspace-apps/useInstallations";
import { installationAppHref } from "../apps/appViewModel";
import { AppGlyph, useModal } from "./shared";
import { appErrorCopy, appLoadErrorCopy, isUnchangedUpgradeError } from "./appErrors";
import { automaticUpgradeClaim, installConsentAllowsAutomatic, mayApplyAutomatically, recordedPackageSource, releaseChangesReach } from "./automaticUpdates";
import { Loading, Notice } from "../app-shell/shared/States";
import { useTranslate } from "../../lib/locale";
import "./explorer.css";

/**
 * The operator's Manage tab for one install (CAD-1129,
 * `/apps/manage/<install_id>`): update check, connections/setup,
 * access on/off and remove. Operator-only — the page itself refuses
 * non-operators and the daemon's `operator_connection` gate is the
 * second layer.
 */
export default function ManageApp({ installId, viewer }: { installId: string; viewer: Viewer }) {
  const t = useTranslate();
  const [inst, setInst] = useState<Installation | null>(null);
  const [home, setHome] = useState<HomeInstallation | null>(null);
  const [preview, setPreview] = useState<RemovePreview | null>(null);
  const [removeOpen, setRemoveOpen] = useState(false);
  const [removing, setRemoving] = useState(false);
  const [restoring, setRestoring] = useState(false);
  const [checking, setChecking] = useState(false);
  const [applying, setApplying] = useState(false);
  const [upgradeSource, setUpgradeSource] = useState("");
  const [automatic, setAutomatic] = useState(false);
  const [automaticReady, setAutomaticReady] = useState(false);
  const [savingAutomatic, setSavingAutomatic] = useState(false);
  const [armed, setArmed] = useState(false);
  const automaticRef = useRef(false);
  const preferenceWrite = useRef(0);
  const autoCheckKey = useRef("");
  const checkRef = useRef<(source: string, mode: "automatic" | "manual") => void>(() => {});
  const [upgradeProposal, setUpgradeProposal] = useState<{
    installId: string;
    source: string;
    expectedDigest: string;
    expectedGeneration: string;
    receipt: UpgradeProposal;
  } | null>(null);
  const currentInstallId = useRef(installId);
  currentInstallId.current = installId;
  const checkRequest = useRef(0);
  const applyRequest = useRef(0);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const isOp = viewer.operator === true && !viewer.readOnly;
  const [reread, setReread] = useState(0);
  // CAD-1438: "Open its page" spells the canonical `/apps/<key>` route,
  // resolved against the verified install list; ambiguous or unsafe
  // keys get no link instead of the retired ID route.
  const installs = useInstallations(viewer.operator === true, reread).list;
  useEffect(() => {
    if (!isOp) return;
    const controller = new AbortController();
    setError(null);
    void workspaceApps.detail(installId, controller.signal)
      .then((d) => { if (!controller.signal.aborted) setInst(d); })
      .catch((e: unknown) => { if (!controller.signal.aborted) setError(appLoadErrorCopy(e, "Could not load the app. Try again in a moment.")); });
    void appExplorer.home(controller.signal)
      .then((h) => {
        if (controller.signal.aborted) return;
        setHome(h.installations.find((i) => i.install_id === installId) ?? null);
      })
      .catch(() => {});
    return () => controller.abort();
  }, [installId, reread, isOp]);

  useEffect(() => {
    if (!isOp) return;
    const controller = new AbortController();
    setAutomatic(false);
    automaticRef.current = false;
    setArmed(false);
    setAutomaticReady(false);
    preferenceWrite.current += 1;
    setSavingAutomatic(false);
    autoCheckKey.current = "";
    void appExplorer.automaticUpdates(installId, controller.signal)
      .then((row) => {
        if (controller.signal.aborted) return;
        setAutomatic(row.automatic);
        automaticRef.current = row.automatic;
        setArmed(row.automatic);
        setAutomaticReady(true);
      })
      .catch(() => { if (!controller.signal.aborted) setAutomaticReady(false); });
    return () => controller.abort();
  }, [installId, isOp]);

  useEffect(() => {
    // The saved preference is armed only after the server confirms it.
    // Opening the page with it already on checks once per package digest;
    // turning it off clears that mark so the next on checks again.
    if (!isOp || !armed) {
      if (!armed) autoCheckKey.current = "";
      return;
    }
    if (!inst || !installConsentAllowsAutomatic(inst)) return;
    const source = recordedPackageSource(inst.source);
    if (!source) return;
    const key = [installId, source, inst.digest, inst.catalog_generation].join("\0");
    if (autoCheckKey.current === key) return;
    autoCheckKey.current = key;
    checkRef.current(source, "automatic");
  }, [armed, installId, inst, isOp]);

  useEffect(() => {
    if (!inst) return;
    // CAD-1422: a `builtin:` source is retired — there is no embedded
    // upstream to prefill. Git installs keep their recorded URL.
    const source = inst.source?.kind === "git" ? inst.source.url ?? "" : "";
    setUpgradeSource(source);
    setUpgradeProposal(null);
  }, [inst]);

  if (viewer.operator === null) {
    return <main className="apps-detail px-4 lg:px-8 pt-4 pb-9" aria-label={t("manage app")}>
      {viewer.access === "unavailable" ? (
        <Notice state="access-unavailable" onRetry={viewer.onRetryAccess} retryLabel="Retry access check">
          {t("Access could not be confirmed. Retry the access check before acting.")}
        </Notice>
      ) : <Loading>{t("Checking whether this session can act…")}</Loading>}
    </main>;
  }
  if (!isOp) {
    return <main className="apps-detail px-4 lg:px-8 pt-4 pb-9">
      <div className="card px-4 py-5">
        <h2 className="font-medium text-ink-100 mb-1">{t("Manage is the operator's")}</h2>
        <p className="text-label text-ink-400">{t("Ask an admin to change access, updates or remove.")}</p>
      </div>
    </main>;
  }
  if (error) {
    return <main className="apps-detail px-4 lg:px-8 pt-4 pb-9">
      <div className="card px-4 py-5" role="alert">
        <h2 className="font-medium text-ink-100 mb-1">{t("Couldn't load the app")}</h2>
        <p className="text-label text-ink-400 mb-3">{t(error)}</p>
        <Link href="/apps" className="btn">{t("Back to Apps")}</Link>
      </div>
    </main>;
  }
  if (!inst) {
    return <main className="apps-detail px-4 lg:px-8 pt-4 pb-9"><p className="text-label text-ink-400" role="status">{t("Loading…")}</p></main>;
  }

  // The record's own mark is authoritative; the home row only agrees with it.
  const removed = inst.removed != null || home?.attention.state === "removed";
  const restorable = inst.restore_after == null || Date.now() / 1000 <= inst.restore_after;
  const off = !removed && (home?.attention.state === "off" || inst.approved === false);
  const updateReady = home?.attention.state === "update";

  const openRemove = () => {
    void appExplorer.removePreview(installId)
      .then(setPreview)
      .catch((e: unknown) => setNotice(appErrorCopy(e, "Couldn't work out what removing would take. Try again in a moment.")));
    setRemoveOpen(true);
  };
  const doRemove = () => {
    if (!preview) return;
    setRemoving(true); setNotice(null);
    void appExplorer.remove(installId, {
      expected_generation: preview.generation,
      expected_digest: preview.digest,
      request_id: `rm-${Date.now()}`,
    })
      .then(() => {
        const now = Math.floor(Date.now() / 1000);
        setRemoveOpen(false);
        setNotice("Removed — it's under Recently removed for 30 days.");
        setInst({ ...inst, approved: false, removed: now, restore_after: now + 30 * 24 * 3600 });
        setHome((h) => h && { ...h, attention: { state: "removed", message: "Removed — restore within 30 days.", action: "restore", count: 0 } });
        notifyAppsChanged();
      })
      .catch((e: unknown) => { setRemoveOpen(false); setNotice(appErrorCopy(e, "Remove didn't finish. Nothing was removed.")); setReread((n) => n + 1); })
      .finally(() => setRemoving(false));
  };
  const restore = () => {
    setRestoring(true); setNotice(null);
    void appExplorer.restore(installId)
      .then(() => {
        setNotice("Restored — it's live again.");
        setInst({ ...inst, removed: null, restore_after: null });
        setHome((h) => h && { ...h, attention: { state: "ok", message: null, action: null, count: 0 } });
        notifyAppsChanged();
      })
      .catch((e: unknown) => {
        setNotice(appErrorCopy(e, "Restore didn't finish. Try again in a moment."));
        // A refusal means the record is not what the page assumed (the window
        // closed, or it is already live): read it again rather than guess.
        setReread((n) => n + 1);
      })
      .finally(() => setRestoring(false));
  };

  const applyKnown = (proposal: {
    installId: string;
    source: string;
    expectedDigest: string;
    expectedGeneration: string;
    receipt: UpgradeProposal;
  }, mode: "automatic" | "manual") => {
    const { source, receipt, expectedDigest, expectedGeneration } = proposal;
    const requestedInstallId = proposal.installId;
    const request = ++applyRequest.current;
    const isCurrentRequest = () => currentInstallId.current === requestedInstallId && applyRequest.current === request;
    setApplying(true); setNotice(null);
    void workspaceApps.upgrade(requestedInstallId, {
      source,
      expected_digest: expectedDigest,
      expected_generation: expectedGeneration,
      expected_new_digest: receipt.digest,
      request_id: `upgrade-${crypto.randomUUID()}`,
      catalogue_resolution: "manual",
      ...(mode === "automatic" ? automaticUpgradeClaim() : {}),
    })
      .then(async () => {
        await appExplorer.updateCheck(requestedInstallId).catch(() => undefined);
        if (!isCurrentRequest()) return;
        setNotice("Update applied."); setUpgradeProposal(null);
        notifyAppsChanged(); setReread((n) => n + 1);
      })
      .catch((e: unknown) => {
        if (!isCurrentRequest()) return;
        setNotice(appErrorCopy(e, "The update wasn't applied. Check the current package and try again."));
        setReread((n) => n + 1);
      })
      .finally(() => { if (isCurrentRequest()) setApplying(false); });
  };
  const checkPackageUpdate = (source: string, mode: "automatic" | "manual") => {
    if (!source) return;
    const requestedInstallId = installId;
    const request = ++checkRequest.current;
    const expectedDigest = inst.digest;
    const expectedGeneration = inst.catalog_generation;
    const recorded = recordedPackageSource(inst.source);
    const isCurrentRequest = () => currentInstallId.current === requestedInstallId && checkRequest.current === request;
    setChecking(true); setNotice(null); setUpgradeProposal(null);
    void workspaceApps.upgradeCheck(requestedInstallId, {
      source, expected_digest: expectedDigest, expected_generation: expectedGeneration,
      catalogue_resolution: "manual",
    })
      .then(async (receipt) => {
        if (!isCurrentRequest()) return;
        const proposal = { installId: requestedInstallId, source, expectedDigest, expectedGeneration, receipt };
        setUpgradeProposal(proposal);
        // Check update, and any source other than the recorded one, wait
        // for Apply. Automatic apply re-reads the saved switch and the
        // install's consent: a stale tab must not turn a revoked app back
        // on or apply after the switch was turned off.
        if (mode !== "automatic" || !mayApplyAutomatically({ checkedSource: source, recordedSource: recorded, receipt })) return;
        const [fresh, preference] = await Promise.all([
          workspaceApps.detail(requestedInstallId),
          appExplorer.automaticUpdates(requestedInstallId),
        ]);
        if (!isCurrentRequest() || !preference.automatic || !installConsentAllowsAutomatic(fresh)) return;
        // The re-read above finishes before the server fetches the package.
        // Carry the approval it saw; the server refuses the commit if that
        // consent or the switch changed during the fetch.
        applyKnown(proposal, "automatic");
      })
      .catch((e: unknown) => {
        if (!isCurrentRequest()) return;
        setNotice(appErrorCopy(e, "The update check didn't finish. Try again in a moment."));
        if (!isUnchangedUpgradeError(e)) setReread((n) => n + 1);
      })
      .finally(() => { if (isCurrentRequest()) setChecking(false); });
  };
  checkRef.current = (source, mode) => { checkPackageUpdate(source, mode); };
  const proposalMatchesCurrent = upgradeProposal != null
    && upgradeProposal.installId === installId
    && upgradeProposal.source === upgradeSource.trim()
    && upgradeProposal.expectedDigest === inst.digest
    && upgradeProposal.expectedGeneration === inst.catalog_generation;
  const toggleAutomatic = () => {
    if (!automaticReady || removed || checking || applying || savingAutomatic) return;
    const next = !automatic;
    const requestedInstallId = installId;
    const confirmed = automaticRef.current;
    const write = ++preferenceWrite.current;
    setSavingAutomatic(true);
    setAutomatic(next);
    // Turning it off stops an in-flight automatic apply immediately. Turning
    // it on does not: the server row is what arms the next check.
    if (!next) {
      automaticRef.current = false;
      setArmed(false);
    }
    void appExplorer.setAutomaticUpdates(requestedInstallId, next)
      .then((row) => {
        if (preferenceWrite.current !== write || currentInstallId.current !== requestedInstallId) return;
        setAutomatic(row.automatic);
        automaticRef.current = row.automatic;
        setArmed(row.automatic);
      })
      .catch((e: unknown) => {
        if (preferenceWrite.current !== write || currentInstallId.current !== requestedInstallId) return;
        setAutomatic(confirmed);
        automaticRef.current = confirmed;
        setArmed(confirmed);
        setNotice(appErrorCopy(e, "Automatic updates were not changed. Try again in a moment."));
      })
      .finally(() => {
        if (preferenceWrite.current === write) setSavingAutomatic(false);
      });
  };

  return (
    <main className="apps-detail px-4 lg:px-8 pt-4 pb-9 min-w-0" aria-label={`Manage ${inst.title}`}>
      <Link href="/apps" className="text-label text-ink-400 hover:text-ink-100">{t("← Apps")}</Link>
      <div className="hero mt-3">
        <AppGlyph name={inst.name} size="lg" />
        <div className="min-w-0">
          <h1>{t("Manage {title}").replace("{title}", () => inst.title)}</h1>
          <p className="tag">{inst.summary}</p>
        </div>
      </div>

      {notice && <div className="alert info mt-4" role="status">{t(notice)}</div>}
      {removed && <div className="alert warn mt-4" role="status">{restorable ? t("Removed. Restore brings it back within its 30-day window.") : t("Removed. The 30-day restore window has closed, so it stays removed.")}</div>}

      <div className="mgrid mt-5">
        <div className="card mcard">
          <h3>{t("App package")}</h3>
          <div className="swrow">
            <div>
              <p>{t("Automatic updates")}</p>
              {automatic && <p className="hint">{t("Updates that change what this app can reach still ask you.")}</p>}
            </div>
            <button
              className="switch"
              type="button"
              role="switch"
              aria-checked={automatic}
              aria-label={t("Automatic updates for {title}").replace("{title}", () => inst.title)}
              disabled={!automaticReady || removed || checking || applying || savingAutomatic}
              onClick={toggleAutomatic}
            />
          </div>
          <p className="mt-3">{updateReady ? t("An update is ready to review.") : t("No update is currently flagged.")}</p>
          <p className="mt-1">{t("Installed version {version}.").replace("{version}", () => inst.version)}</p>
          {inst.source?.kind === "builtin" && (
            <p className="hint mt-2">{t("This install came from the retired built-in catalog — name the app's detached repository (a path or Git URL) to update it.")}</p>
          )}
          <label className="block mt-3 text-label text-ink-400">
            <span className="block mb-1">{t("App package path or Git URL")}</span>
            <input className="update-source" type="text" value={upgradeSource}
              onChange={(event) => { setUpgradeSource(event.target.value); setUpgradeProposal(null); }}
              placeholder="/absolute/path or Git URL" autoComplete="off" />
          </label>
          <div className="mt-3">
            <Button className="btn-sm" disabled={checking || applying || !upgradeSource.trim()} onClick={() => checkPackageUpdate(upgradeSource.trim(), "manual")}>{checking ? t("Checking…") : t("Check update")}</Button>
          </div>
          {proposalMatchesCurrent && upgradeProposal && <div className="mt-3" aria-live="polite">
            <p>{t("Proposed version {version} · {name}").replace("{version}", () => upgradeProposal.receipt.version).replace("{name}", () => upgradeProposal.receipt.name)}</p>
            <p className="hint mt-2">{t("New digest:")} <code className="break-all">{upgradeProposal.receipt.digest}</code></p>
            {(["added", "changed", "removed"] as const).map((kind) => <p className="hint mt-1" key={kind}>{kind}: {upgradeProposal.receipt.structural_diff[kind].join(", ") || "none"}</p>)}
            {upgradeProposal.receipt.compatibility != null && <CompatibilityLines receipt={upgradeProposal.receipt.compatibility} />}
            {!!upgradeProposal.receipt.notes?.length && <ul className="hint mt-2">{upgradeProposal.receipt.notes.map((note, index) => <li key={index}>{note}</li>)}</ul>}
            {!!upgradeProposal.receipt.secret_warnings?.length && <p className="alert warn mt-2">{t("Package validation reported secret warnings; review the source before applying.")}</p>}
            {automatic && releaseChangesReach(upgradeProposal.receipt.access_diff) && <p className="hint mt-2">{t("This release changes what {title} can reach.").replace("{title}", () => inst.title)}</p>}
            <div className="mt-3"><Button className="btn-primary btn-sm" disabled={!proposalMatchesCurrent || applying || !!upgradeProposal.receipt.secret_warnings?.length} onClick={() => applyKnown(upgradeProposal, "manual")}>{applying ? t("Applying…") : t("Apply checked update")}</Button></div>
          </div>}
        </div>

        <div className="card mcard">
          <h3>{t("Connections and setup")}</h3>
          <p>{t("The accounts and helpers {title} uses.").replace("{title}", () => inst.title)}</p>
          {(inst.connection_slots ?? []).length === 0 ? (
            <p className="hint mt-2">{t("No outside accounts.")}</p>
          ) : (
            <ul className="mt-2">
              {inst.connection_slots.map((s) => (
                <li key={s} className="conn"><span>◦</span><span className="grow">{s}</span></li>
              ))}
            </ul>
          )}
          <div className="mt-3">
            {(() => {
              const href = installationAppHref(installs, inst.install_id);
              return href === null ? (
                <span className="text-micro text-ink-500">{t("No unique app page — the app key names more than one installation.")}</span>
              ) : (
                <Link className="btn btn-sm" href={href}>{t("Open its page")}</Link>
              );
            })()}
          </div>
        </div>

        <div className="card mcard">
          <h3>{t("Access")}</h3>
          <div className="swrow">
            <div>
              <p>{t("Let {title} work").replace("{title}", () => inst.title)}</p>
              <p className="hint">{removed ? t("Off — the app is removed.") : off ? t("Off — it can't run or use its connections. Its data is kept.") : t("On — it can run and use its connections.")}</p>
            </div>
            <button
              className="switch"
              role="switch"
              aria-checked={!off && !removed}
              aria-label={`Let ${inst.title} work`}
              onClick={() => setNotice(off ? "Turn on through the app's page — consent is recorded there." : "Turn off through the app's page — consent is revoked there.")}
            />
          </div>
        </div>

        <div className="card mcard">
          {removed ? (
            <>
              <h3>{t("Restore {title}").replace("{title}", () => inst.title)}</h3>
              <p>{restorable ? t("Removed apps restore within 30 days, with the same data.") : t("The restore window has closed.")}</p>
              {restorable && (
                <div className="mt-3">
                  <Button className="btn-primary btn-sm" disabled={restoring} onClick={restore}>{restoring ? t("Restoring…") : t("Restore")}</Button>
                </div>
              )}
            </>
          ) : (
            <>
              <h3 className="text-fail">{t("Remove {title}").replace("{title}", () => inst.title)}</h3>
              <p>{t("Takes the app and its data out of this workspace. You'll see exactly what goes before anything is deleted.")}</p>
              <div className="mt-3">
                <Button className="btn-danger btn-sm" onClick={openRemove}>{t("Remove app…")}</Button>
              </div>
            </>
          )}
        </div>
      </div>

      {removeOpen && preview && (
        <RemoveDialog preview={preview} removing={removing} onCancel={() => setRemoveOpen(false)} onConfirm={doRemove} />
      )}
    </main>
  );
}

/** Render the daemon's compatibility receipt as one readable line per requirement. */
function CompatibilityLines({ receipt }: { receipt: UpgradeProposal["compatibility"] }) {
  const t = useTranslate();
  if (!receipt) return null;
  if (!receipt.declared) return <p className="hint mt-2">{t("No host requirements declared.")}</p>;
  if (receipt.status === "unknown") return <p className="hint mt-2">{t("Compatibility could not be determined.")}</p>;
  if (receipt.checks.length === 0) return <p className="hint mt-2">{t("Compatibility requirements could not be read.")}</p>;
  return <ul className="hint mt-2" aria-label={t("Package compatibility")}>
    {receipt.checks.map((check, index) => {
      if (check.kind === "core") {
        const required = typeof check.required === "string" ? check.required : check.required.join(", ");
        return <li key={`core-${index}`}>Needs Cadence {required} — this host {check.host ?? "unknown"} {check.ok ? "✓" : "✗"}</li>;
      }
      const required = Array.isArray(check.required) ? check.required : [check.required];
      const supported = check.supported ?? [];
      return <li key={`contract-${check.contract ?? index}`}>
        Needs {check.contract ?? "host contract"} {required.map((major) => `v${major}`).join(", ")} — {check.ok
          ? "supported ✓"
          : `not supported (host supports ${supported.length ? supported.map((major) => `v${major}`).join(", ") : "none"}) ✗`}
      </li>;
    })}
  </ul>;
}

/**
 * The Remove confirmation: an alertdialog that takes focus, keeps Tab
 * inside and closes on Escape or Cancel (CAD-1209). Mounted only while
 * open so its focus handling starts and ends with it.
 */
function RemoveDialog({ preview, removing, onCancel, onConfirm }: {
  preview: RemovePreview; removing: boolean; onCancel: () => void; onConfirm: () => void;
}) {
  const t = useTranslate();
  const ref = useModal<HTMLDivElement>(true, onCancel);
  return (
    <div className="scrim fixed inset-0 bg-black/50 z-50 grid place-items-center p-4" role="presentation" onClick={onCancel}>
      <div ref={ref} tabIndex={-1} className="modal card p-5 max-w-lg w-full" role="alertdialog" aria-modal="true"
        aria-labelledby="rmh" aria-describedby="rmd" onClick={(e) => e.stopPropagation()}>
        <h2 id="rmh" className="text-cardtitle font-medium text-ink-100 mb-3">Remove {preview.title}?</h2>
        <div id="rmd">
          {preview.personal_data && (
            <div className="alert warn mb-3">This app holds personal data. Its data is kept for restore.</div>
          )}
          <p className="text-label text-ink-400 mb-2">This is taken out of this workspace; its data is kept for 30 days, then deleted for good.</p>
          {preview.keeps?.data && <p className="text-label text-ink-500 mb-3">Keeps: {preview.keeps.data}</p>}
          <div className="alert info mb-4">Restorable for 30 days. Bring it back any time from Recently removed on the Apps page.</div>
          <p className="text-label text-ink-500 mb-4">Only want to pause it? Turn access off from the app's page instead — nothing is removed.</p>
        </div>
        <div className="flex gap-2 justify-end">
          <Button onClick={onCancel}>{t("Cancel")}</Button>
          <Button className="btn-danger-solid" disabled={removing} onClick={onConfirm}>
            {removing ? t("Removing…") : t("Remove {title}").replace("{title}", () => preview.title)}
          </Button>
        </div>
      </div>
    </div>
  );
}
