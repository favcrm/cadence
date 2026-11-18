import { useEffect, useRef, useState } from "react";
import { ApiError } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import { onPrivateReadsPaused, onPrivateSessionReset } from "../../lib/privateSession";
import Button from "../../ui/Button";
import { workspaceApps, type AppAgentSelection } from "./workspaceApps";

type Failure = "sign-in" | "unavailable";
type Snapshot = { binding: AppAgentSelection; profiles: Map<string, string> };

export function AppAgentSettings({ installId, busy }: { installId: string; busy: boolean }) {
  const t = useTranslate();
  const inFlight = useRef<AbortController | null>(null);
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  // Undefined follows the current server value. An empty string is an explicit clear.
  const [draft, setDraft] = useState<string>();
  const [failure, setFailure] = useState<Failure | null>(null);
  const [pending, setPending] = useState(true);
  const [notice, setNotice] = useState<"conflict" | "unconfirmed" | "saved" | null>(null);
  const [reload, setReload] = useState(0);

  useEffect(() => {
    const controller = new AbortController();
    inFlight.current = controller;
    const clearPrivate = () => {
      inFlight.current?.abort();
      inFlight.current = null;
      setSnapshot(null);
      setDraft(undefined);
      setNotice(null);
      setFailure("sign-in");
      setPending(false);
    };
    const stopPause = onPrivateReadsPaused(clearPrivate);
    const stopReset = onPrivateSessionReset(clearPrivate);
    void (async () => {
      try {
        // Read the private binding first; a host that doesn't offer it need not read the roster.
        const binding = await workspaceApps.appAgent(installId, controller.signal);
        const profiles = await workspaceApps.activeAgentProfiles(controller.signal);
        if (!controller.signal.aborted) setSnapshot({ binding, profiles });
      } catch (error) {
        if (!controller.signal.aborted) setFailure(error instanceof ApiError && error.status === 401 ? "sign-in" : "unavailable");
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
  }, [installId, reload]);

  const binding = snapshot?.binding;
  const committed = binding?.agent?.id ?? "";
  const value = draft ?? committed;
  const dirty = draft !== undefined && (draft !== committed || binding?.stale === true);
  const canSave = !!snapshot && !pending && !busy && (dirty || binding?.stale === true) &&
    (value === "" || snapshot.profiles.has(value));

  async function save() {
    if (!canSave || !snapshot || inFlight.current) return;
    const controller = new AbortController();
    inFlight.current = controller;
    setPending(true);
    setNotice(null);
    try {
      const binding = await workspaceApps.setAppAgent(installId, snapshot.binding, value || null, controller.signal);
      if (controller.signal.aborted) return;
      setSnapshot({ ...snapshot, binding });
      setDraft(undefined);
      setNotice("saved");
    } catch (error) {
      if (controller.signal.aborted) return;
      // A 409 or a lost/malformed acknowledgement is not proof the write did
      // nothing. Drop the old receipt, keep the requested choice, read back,
      // and require a new explicit Save before applying it again.
      setSnapshot(null);
      setDraft(value);
      if (error instanceof ApiError && [401, 403].includes(error.status)) {
        setDraft(undefined);
        setFailure(error.status === 401 ? "sign-in" : "unavailable");
        return;
      }
      setNotice(error instanceof ApiError && error.status === 409 ? "conflict" : "unconfirmed");
      setReload(count => count + 1);
    } finally {
      if (inFlight.current === controller) {
        inFlight.current = null;
        setPending(false);
      }
    }
  }

  return (
    <section className="card mcard wa-stack" aria-label={t("Default app agent")}>
      <h3>{t("Default app agent")}</h3>
      <p className="wa-muted">{t("This choice applies only to your app conversations. It does not change tool access.")}</p>
      {failure && <div className="wa-stack">
        <p className="wa-alert" role="alert">{failure === "sign-in" ? t("Sign in again to manage your default app agent.") : t("Default app agent settings are unavailable on this board.")}</p>
        <Button onClick={() => { setFailure(null); setPending(true); setReload(count => count + 1); }}>{t("Retry")}</Button>
      </div>}
      {pending && !snapshot && !failure && <p role="status">{t("Loading…")}</p>}
      {notice && <p role="status" className={notice === "saved" ? "wa-muted" : "wa-alert"}>{
        notice === "saved" ? t("Default app agent saved.") : notice === "conflict" ?
          t("The app or selection changed. Review the current setting before saving again.") :
          t("The save could not be confirmed. Check the current setting before trying again.")
      }</p>}
      {snapshot && <form className="wa-stack" onSubmit={event => { event.preventDefault(); void save(); }}>
        <p className="wa-muted">{snapshot.binding.stale ? t("The previous selection is no longer current. Choose an active profile or clear it.") : snapshot.binding.agent ?
          t("Current agent: {name}").replace("{name}", () => snapshot.binding.agent!.name) : t("No default agent is selected.")}</p>
        {snapshot.profiles.size === 0 && <p className="wa-muted">{t("No active agent profiles are available.")}</p>}
        <label className="wa-field">
          <span>{t("Agent profile")}</span>
          <select name="profile_id" value={value} disabled={busy || pending} onChange={event => {
            setNotice(null);
            setDraft(event.target.value);
          }}>
            <option value="">{t("No default agent")}</option>
            {value && !snapshot.profiles.has(value) && <option value={value} disabled>{t("Profile unavailable")}</option>}
            {[...snapshot.profiles].map(([id, name]) => <option key={id} value={id}>{name}</option>)}
          </select>
        </label>
        {dirty && <p className="wa-muted">{t("Selection not saved.")}</p>}
        <Button type="submit" variant="primary" loading={pending} disabled={!canSave}>{t("Save changes")}</Button>
      </form>}
    </section>
  );
}
