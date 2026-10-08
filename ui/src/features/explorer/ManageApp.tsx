import { useEffect, useState } from "react";
import Link from "../../ui/Link";
import Button from "../../ui/Button";
import {
  appExplorer,
  workspaceApps,
  type HomeInstallation,
  type Installation,
  type RemovePreview,
} from "../workspace-apps/workspaceApps";
import type { Viewer } from "../projects/work";
import { AppGlyph } from "./shared";
import "./explorer.css";

/**
 * The operator's Manage tab for one install (CAD-1129,
 * `/apps/manage/<install_id>`): update check, connections/setup,
 * access on/off and remove. Operator-only — the page itself refuses
 * non-operators and the daemon's `operator_connection` gate is the
 * second layer.
 */
export default function ManageApp({ installId, viewer }: { installId: string; viewer: Viewer }) {
  const [inst, setInst] = useState<Installation | null>(null);
  const [home, setHome] = useState<HomeInstallation | null>(null);
  const [preview, setPreview] = useState<RemovePreview | null>(null);
  const [removeOpen, setRemoveOpen] = useState(false);
  const [removing, setRemoving] = useState(false);
  const [restoring, setRestoring] = useState(false);
  const [checking, setChecking] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const isOp = viewer.operator && !viewer.readOnly;

  useEffect(() => {
    const controller = new AbortController();
    setError(null);
    void workspaceApps.detail(installId, controller.signal)
      .then((d) => { if (!controller.signal.aborted) setInst(d); })
      .catch((e: unknown) => { if (!controller.signal.aborted) setError(e instanceof Error ? e.message : "Could not load the install"); });
    void appExplorer.home(controller.signal)
      .then((h) => {
        if (controller.signal.aborted) return;
        setHome(h.installations.find((i) => i.install_id === installId) ?? null);
      })
      .catch(() => {});
    return () => controller.abort();
  }, [installId]);

  if (!isOp) {
    return <main className="apps-detail px-4 lg:px-8 pt-4 pb-9">
      <div className="card px-4 py-5">
        <h2 className="font-medium text-ink-100 mb-1">Manage is the operator's</h2>
        <p className="text-label text-ink-400">Ask an admin to change access, updates or remove.</p>
      </div>
    </main>;
  }
  if (error) {
    return <main className="apps-detail px-4 lg:px-8 pt-4 pb-9">
      <div className="card px-4 py-5" role="alert">
        <h2 className="font-medium text-ink-100 mb-1">Couldn't load the app</h2>
        <p className="text-label text-ink-400 mb-3">{error}</p>
        <Link href="/apps" className="btn">Back to Apps</Link>
      </div>
    </main>;
  }
  if (!inst) {
    return <main className="apps-detail px-4 lg:px-8 pt-4 pb-9"><p className="text-label text-ink-400" role="status">Loading…</p></main>;
  }

  const removed = home?.attention.state === "removed";
  const off = home?.attention.state === "off" || inst.approved === false;
  const updateReady = home?.attention.state === "update";

  const checkUpdate = () => {
    setChecking(true); setNotice(null);
    void appExplorer.updateCheck(installId)
      .then((r) => setNotice(r.has_update ? `An update is ready (${r.digest.slice(0, 14)}…).` : "Up to date."))
      .catch((e: unknown) => setNotice(e instanceof Error ? e.message : "The check didn't finish"))
      .finally(() => setChecking(false));
  };

  const openRemove = () => {
    void appExplorer.removePreview(installId)
      .then(setPreview)
      .catch((e: unknown) => setNotice(e instanceof Error ? e.message : "Could not preview remove"));
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
      .then(() => { setRemoveOpen(false); setNotice("Removed — it's under Recently removed for 30 days."); setInst({ ...inst, approved: false }); })
      .catch((e: unknown) => setNotice(e instanceof Error ? e.message : "Remove didn't finish"))
      .finally(() => setRemoving(false));
  };
  const restore = () => {
    setRestoring(true); setNotice(null);
    void appExplorer.restore(installId)
      .then(() => setNotice("Restored — it's live again."))
      .catch((e: unknown) => setNotice(e instanceof Error ? e.message : "Restore was refused"))
      .finally(() => setRestoring(false));
  };

  const isGit = typeof inst.source === "object" && inst.source !== null && "kind" in inst.source && (inst.source as { kind?: string }).kind === "git";

  return (
    <main className="apps-detail px-4 lg:px-8 pt-4 pb-9 min-w-0" aria-label={`Manage ${inst.title}`}>
      <Link href="/apps" className="text-label text-ink-400 hover:text-ink-100">← Apps</Link>
      <div className="hero mt-3">
        <AppGlyph name={inst.name} size="lg" />
        <div className="min-w-0">
          <h1>Manage {inst.title}</h1>
          <p className="tag">{inst.summary}</p>
        </div>
      </div>

      {notice && <div className="alert info mt-4" role="status">{notice}</div>}
      {removed && <div className="alert warn mt-4">Removed. Restore brings it back within its 30-day window.</div>}

      <div className="mgrid mt-5">
        <div className="card mcard">
          <h3>{updateReady ? "Update ready" : "Up to date"}</h3>
          <p>Version {inst.version}.</p>
          {isGit && (
            <div className="mt-3">
              <Button className="btn-sm" disabled={checking} onClick={checkUpdate}>{checking ? "Checking…" : "Check now"}</Button>
            </div>
          )}
        </div>

        <div className="card mcard">
          <h3>Connections and setup</h3>
          <p>The accounts and helpers {inst.title} uses.</p>
          {(inst.connection_slots ?? []).length === 0 ? (
            <p className="hint mt-2">No outside accounts.</p>
          ) : (
            <ul className="mt-2">
              {inst.connection_slots.map((s) => (
                <li key={s} className="conn"><span>◦</span><span className="grow">{s}</span></li>
              ))}
            </ul>
          )}
          <div className="mt-3">
            <Link className="btn btn-sm" href={`/app-installations/${inst.install_id}`}>Open its page</Link>
          </div>
        </div>

        <div className="card mcard">
          <h3>Access</h3>
          <div className="swrow">
            <div>
              <p>Let {inst.title} work</p>
              <p className="hint">{off ? "Off — it can't run or use its connections. Its data is kept." : "On — it can run and use its connections."}</p>
            </div>
            <button
              className="switch"
              role="switch"
              aria-checked={!off}
              aria-label={`Let ${inst.title} work`}
              onClick={() => setNotice(off ? "Turn on through the app's page — consent is recorded there." : "Turn off through the app's page — consent is revoked there.")}
            />
          </div>
        </div>

        <div className="card mcard">
          {removed ? (
            <>
              <h3>Restore {inst.title}</h3>
              <p>Removed apps restore within 30 days, with the same data.</p>
              <div className="mt-3">
                <Button className="btn-primary btn-sm" disabled={restoring} onClick={restore}>{restoring ? "Restoring…" : "Restore"}</Button>
              </div>
            </>
          ) : (
            <>
              <h3 className="text-fail">Remove {inst.title}</h3>
              <p>Takes the app and its data out of this workspace. You'll see exactly what goes before anything is deleted.</p>
              <div className="mt-3">
                <Button className="btn-danger btn-sm" onClick={openRemove}>Remove app…</Button>
              </div>
            </>
          )}
        </div>
      </div>

      {removeOpen && preview && (
        <div className="scrim fixed inset-0 bg-black/50 z-50 grid place-items-center p-4" role="presentation" onClick={() => setRemoveOpen(false)}>
          <div className="modal card p-5 max-w-lg w-full" role="alertdialog" aria-modal="true" aria-labelledby="rmh" onClick={(e) => e.stopPropagation()}>
            <h2 id="rmh" className="text-cardtitle font-medium text-ink-100 mb-3">Remove {preview.title}?</h2>
            {preview.personal_data && (
              <div className="alert warn mb-3">This app holds personal data. Its data is kept for restore.</div>
            )}
            <p className="text-label text-ink-400 mb-2">This is taken out of this workspace; its data is kept for 30 days, then deleted for good.</p>
            {preview.keeps?.data && <p className="text-label text-ink-500 mb-3">Keeps: {preview.keeps.data}</p>}
            <div className="alert info mb-4">Restorable for 30 days. Bring it back any time from Recently removed on the Apps page.</div>
            <p className="text-label text-ink-500 mb-4">Only want to pause it? Turn access off from the app's page instead — nothing is removed.</p>
            <div className="flex gap-2 justify-end">
              <Button onClick={() => setRemoveOpen(false)}>Cancel</Button>
              <Button className="btn-danger-solid" disabled={removing} onClick={doRemove}>
                {removing ? "Removing…" : `Remove ${preview.title}`}
              </Button>
            </div>
          </div>
        </div>
      )}
    </main>
  );
}
