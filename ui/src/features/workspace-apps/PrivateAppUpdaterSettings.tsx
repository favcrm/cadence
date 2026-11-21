import { useEffect, useRef, useState } from "react";
import { ApiError } from "../../lib/apiError";
import { useTranslate } from "../../lib/locale";
import { onPrivateReadsPaused, onPrivateSessionReset } from "../../lib/privateSession";
import Button from "../../ui/Button";
import {
  grantPrivateAppUpdatePolicy,
  readPrivateAppUpdatePolicy,
  revokePrivateAppUpdatePolicy,
  type PrivateAppRoutinePolicy,
} from "./privateAppUpload";
import { workspaceApps } from "./workspaceApps";

const THIRTY_DAYS_MS = 30 * 24 * 60 * 60 * 1000;

type Snapshot = {
  installedDigest: string;
  policy: PrivateAppRoutinePolicy | null;
  profiles: Map<string, string>;
};

export function PrivateAppUpdaterSettings({
  installId,
  installedDigest,
  busy,
}: {
  installId: string;
  installedDigest: string;
  busy: boolean;
}) {
  const t = useTranslate();
  const inFlight = useRef<AbortController | null>(null);
  const [supported, setSupported] = useState(true);
  const [observed, setObserved] = useState<Snapshot | null>(null);
  const [selectedProfile, setSelectedProfile] = useState("");
  const [pending, setPending] = useState(true);
  const [failure, setFailure] = useState<{ message: string; requestId?: string } | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [reload, setReload] = useState(0);

  useEffect(() => {
    const controller = new AbortController();
    inFlight.current = controller;
    setPending(true);
    setFailure(null);
    const clear = () => {
      inFlight.current?.abort();
      inFlight.current = null;
      setObserved(null);
      setNotice(null);
      setPending(false);
    };
    const stopPause = onPrivateReadsPaused(clear);
    const stopReset = onPrivateSessionReset(clear);
    void (async () => {
      try {
        const read = await readPrivateAppUpdatePolicy(installId, controller.signal);
        if (controller.signal.aborted) return;
        if (!read.supported) {
          setSupported(false);
          setObserved(null);
          return;
        }
        setSupported(true);
        const profiles = await workspaceApps.activeAgentProfiles(controller.signal);
        if (controller.signal.aborted) return;
        setObserved({ installedDigest, policy: read.policy, profiles });
        setSelectedProfile((prev) => (prev && profiles.has(prev) ? prev : profiles.keys().next().value ?? ""));
      } catch (cause) {
        if (controller.signal.aborted) return;
        setFailure({
          message: cause instanceof ApiError && cause.status === 403
            ? "Only a current workspace owner can change update automation."
            : cause instanceof ApiError && cause.message
              ? cause.message
              : "Update automation settings are unavailable.",
          requestId: cause instanceof ApiError ? cause.requestId : undefined,
        });
      } finally {
        if (inFlight.current === controller) {
          inFlight.current = null;
          setPending(false);
        }
      }
    })();
    return () => {
      inFlight.current?.abort();
      inFlight.current = null;
      stopPause();
      stopReset();
    };
  }, [installId, installedDigest, reload]);

  if (!supported) return null;

  const packageChanged = observed !== null && observed.installedDigest !== installedDigest;
  const snapshot = packageChanged ? null : observed;
  const policy = snapshot?.policy ?? null;

  const grant = async () => {
    if (!snapshot || !selectedProfile || pending || busy) return;
    setPending(true);
    setFailure(null);
    setNotice(null);
    try {
      const next = await grantPrivateAppUpdatePolicy({
        installId,
        expectedDigest: installedDigest,
        agent: `agents/${selectedProfile}`,
        source: "staged_zip",
        expiresAt: Date.now() + THIRTY_DAYS_MS,
      });
      setObserved({ ...snapshot, policy: next });
      setNotice("Routine patch permission saved for 30 days.");
    } catch (cause) {
      setFailure({
        message: cause instanceof ApiError && cause.message
          ? cause.message
          : "Routine update permission could not be saved.",
        requestId: cause instanceof ApiError ? cause.requestId : undefined,
      });
    } finally {
      setPending(false);
    }
  };

  const revoke = async () => {
    if (!snapshot || !policy || pending || busy) return;
    setPending(true);
    setFailure(null);
    setNotice(null);
    try {
      await revokePrivateAppUpdatePolicy(installId, policy.id);
      setObserved({ ...snapshot, policy: null });
      setNotice("Routine patch permission revoked.");
    } catch (cause) {
      setFailure({
        message: cause instanceof ApiError && cause.message
          ? cause.message
          : "Routine update permission could not be revoked.",
        requestId: cause instanceof ApiError ? cause.requestId : undefined,
      });
    } finally {
      setPending(false);
    }
  };

  return (
    <section className="card mcard wa-stack" aria-label={t("Private app update automation")}>
      <h3>{t("Private app update automation")}</h3>
      <p className="wa-muted">
        {t("Grants one enrolled agent limited permission to apply future routine patch updates without changing Default app agent chat settings. Personal Assistant cannot apply updates.")}
      </p>
      {failure && (
        <div className="wa-stack">
          <p className="wa-alert" role="alert">
            {t(failure.message)}
            {failure.requestId && <span className="ml-2 text-micro num">Ref: {failure.requestId}</span>}
          </p>
          {!snapshot && (
            <Button onClick={() => setReload((count) => count + 1)}>{t("Retry")}</Button>
          )}
        </div>
      )}
      {notice && <p className="wa-muted" role="status">{t(notice)}</p>}
      {(pending || packageChanged) && !snapshot && !failure && <p role="status">{t("Loading…")}</p>}
      {snapshot && policy && (
        <div className="wa-stack">
          <p>
            {t("Updater agent")}: <code>{policy.agent}</code>
          </p>
          <p className="wa-muted">
            {t("Package source")}: {policy.source_kind === "staged_zip" ? t("Staged ZIP") : t("GitHub release")} ·{" "}
            {t("Expires")}: {new Date(policy.expires_at).toLocaleDateString()} ·{" "}
            {policy.installed_digest === installedDigest ? t("Matches current version") : t("Pinned to another version")}
          </p>
          <div>
            <Button className="btn-sm" disabled={pending || busy} onClick={() => void revoke()}>
              {pending ? t("Revoking…") : t("Revoke permission")}
            </Button>
          </div>
        </div>
      )}
      {snapshot && !policy && (
        <div className="wa-stack">
          {snapshot.profiles.size === 0 ? (
            <p className="wa-muted">{t("No active agent profiles are available.")}</p>
          ) : (
            <form className="wa-stack" onSubmit={(event) => { event.preventDefault(); void grant(); }}>
              <label className="text-label text-ink-200">
                {t("Updater agent")}
                <select
                  className="field w-full mt-1"
                  value={selectedProfile}
                  disabled={pending || busy}
                  onChange={(event) => setSelectedProfile(event.currentTarget.value)}
                >
                  {[...snapshot.profiles.entries()].map(([id, name]) => (
                    <option key={id} value={id}>{name} (agents/{id})</option>
                  ))}
                </select>
              </label>
              <div>
                <Button type="submit" className="btn-sm" disabled={!selectedProfile || pending || busy}>
                  {pending ? t("Saving…") : t("Allow routine patch updates (30 days)")}
                </Button>
              </div>
            </form>
          )}
        </div>
      )}
    </section>
  );
}
