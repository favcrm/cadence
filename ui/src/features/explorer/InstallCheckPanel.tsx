import { useCallback, useEffect, useState } from "react";
import Button from "../../ui/Button";
import { appExplorer, type InstallCheck } from "../workspace-apps/workspaceApps";
import { appErrorCopy } from "./appErrors";
import "./explorer.css";

/**
 * The install review (CAD-1194): every install from the Explorer — a
 * built-in, a Git URL — checks first. The panel asks the daemon what
 * the source would install (`install-check`: name, version, digest, the
 * file list, notes and secret warnings; nothing is written), shows it,
 * and only then offers Install, which sends the checked digest. If the
 * bundle changed after the check the daemon refuses and nothing is
 * installed; the panel shows that refusal and offers a fresh check.
 */
export default function InstallCheckPanel({ source, label, install, onInstalled, onClose }: {
  /** `builtin:<id>`, or a Git URL / path the operator named. */
  source: string;
  label: string;
  /** Run the install pinned to `digest`. */
  install: (digest: string) => Promise<unknown>;
  onInstalled: () => void;
  onClose: () => void;
}) {
  const [check, setCheck] = useState<InstallCheck | null>(null);
  const [checking, setChecking] = useState(true);
  const [installing, setInstalling] = useState(false);
  const [checkError, setCheckError] = useState<string | null>(null);
  const [installError, setInstallError] = useState<string | null>(null);

  const runCheck = useCallback((signal?: AbortSignal) => {
    setChecking(true); setCheck(null); setCheckError(null); setInstallError(null);
    void appExplorer.installCheck(source)
      .then((c) => { if (!signal?.aborted) setCheck(c); })
      .catch((e: unknown) => { if (!signal?.aborted) setCheckError(appErrorCopy(e, "Could not check this app. Try again in a moment.")); })
      .finally(() => { if (!signal?.aborted) setChecking(false); });
  }, [source]);

  useEffect(() => {
    const controller = new AbortController();
    runCheck(controller.signal);
    return () => controller.abort();
  }, [runCheck]);

  const doInstall = () => {
    if (!check) return;
    setInstalling(true); setInstallError(null);
    install(check.digest)
      .then(onInstalled)
      .catch((e: unknown) => setInstallError(appErrorCopy(e, "Install didn't finish. Nothing was installed.")))
      .finally(() => setInstalling(false));
  };

  const warnings = check?.secret_warnings ?? [];
  return (
    <div className="card install-check p-4 mb-4" role="dialog" aria-label={`Check ${label} before installing`}>
      <h3 className="font-medium text-ink-100 mb-1">Check {label} before installing</h3>
      {checking && <p className="text-label text-ink-400" role="status">Checking exactly what would be installed…</p>}
      {checkError && (
        <div className="alert fail mt-2" role="alert">
          {checkError}
          <div className="mt-2"><Button className="btn-sm" onClick={() => runCheck()}>Check again</Button></div>
        </div>
      )}
      {check && (
        <>
          <dl className="ic-facts text-label mt-2">
            <dt>App</dt><dd>{check.name}</dd>
            <dt>Version</dt><dd>{check.version}</dd>
            <dt>Digest</dt><dd><code className="ic-digest" title={check.digest}>{check.digest}</code></dd>
            <dt>Files</dt><dd>{check.files.length}</dd>
          </dl>
          <details className="mt-2">
            <summary className="text-label text-ink-400 cursor-pointer">Show the {check.files.length} files</summary>
            <ul className="ic-files text-micro text-ink-400">{check.files.map((f) => <li key={f}><code>{f}</code></li>)}</ul>
          </details>
          {check.notes.length > 0 && (
            <ul className="text-label text-ink-400 mt-2">{check.notes.map((n) => <li key={n}>{n}</li>)}</ul>
          )}
          {warnings.length > 0 && (
            <div className="alert warn mt-2" role="alert">
              {warnings.length} possible secret{warnings.length === 1 ? "" : "s"} found in this app’s files:
              <ul>{warnings.map((w) => <li key={`${w.rule}:${w.line}:${w.column}`}>{w.rule} at line {w.line} ({w.redacted})</li>)}</ul>
            </div>
          )}
          <p className="text-micro text-ink-500 mt-2">Installing records your approval of exactly this digest.</p>
        </>
      )}
      {installError && (
        <div className="alert fail mt-2" role="alert">
          {installError}
          <div className="mt-1 text-micro">Nothing was installed. Check again to review the current version.</div>
          <div className="mt-2"><Button className="btn-sm" onClick={() => runCheck()}>Check again</Button></div>
        </div>
      )}
      <div className="flex gap-2 mt-3">
        <Button className="btn-sm btn-primary" disabled={!check || installing || !!installError} onClick={doInstall}>
          {installing ? "Installing…" : "Install and record approval"}
        </Button>
        <Button className="btn-sm" onClick={onClose}>Cancel</Button>
      </div>
    </div>
  );
}
