import type { Meta } from "../../lib/types";
import DeviceSignIn from "./DeviceSignIn";
import { SIGN_IN_COMMAND } from "./gate";
import { IconLock } from "../../ui/icons";
import { useLocale } from "../../lib/locale";

/**
 * The header's sign-in state: "Sign in with `cadence ui login`" when this
 * browser holds no session, else nothing. The signed-in identity lives in
 * AccountMenu. Explicit anonymous read-only browsing still offers local login.
 */
export default function SignIn({
  meta,
  onChange,
  access,
  recheckFinished,
  onRetryWorkspace,
}: {
  meta: Meta | null;
  onChange: () => void;
  /** CAD-1193: how the board's access check currently reads —
   *  `"unavailable"` when a completed probe could not answer. */
  access?: "checking" | "unavailable" | null;
  /** Shared App-level timer so opening the mobile menu cannot restart the wait. */
  recheckFinished?: boolean;
  /** Reload the board to use the same workspace wake path as page load. */
  onRetryWorkspace?: () => void;
}) {
  const { t } = useLocale();
  // CAD-1193: `signed_in === null` is an unanswerable check —
  // unavailable, never a sign-out. It stays unavailable throughout the
  // bounded re-check and never becomes signed-out or authority.
  if (access === "unavailable" && !meta?.read_only) {
    return (
      <span className="chip inline-flex items-center gap-1.5 bg-warn/10 text-warn" role="status" title={t("Workspace waking or unreachable")}>
        <span className="sr-only sm:hidden">{t("Workspace waking or unreachable.")}</span>
        <span className="sm:hidden">{t("Workspace waking")}</span>
        <span className="hidden sm:inline">{t("Workspace waking or unreachable.")} {recheckFinished ? t("Retry when ready.") : t("Checking again for up to 60 seconds…")}</span>
        {recheckFinished && onRetryWorkspace && (
          <button type="button" className="lnk whitespace-nowrap text-warn" onClick={onRetryWorkspace}>
            {t("Retry workspace")}
          </button>
        )}
      </span>
    );
  }
  if (!meta || meta.signed_in === undefined || meta.signed_in === null) return null;
  if (!meta.signed_in) {
    const cmd = meta.login_hint ?? SIGN_IN_COMMAND;
    const device = meta.device_login === true;
    return (
      <details className="relative text-label">
        {/* CAD-1312: unsigned-in browsing is a neutral state, not a warning —
            the quiet pill carries the lock, "Read only" and the accent action. */}
        <summary className="header-auth-target header-state" aria-label={t(meta.read_only ? "Sign in to your local board." : "Read-only. Sign in to make changes")}>
          <IconLock size={11} />
          <span className="hidden sm:inline">{t("Read only")}<span className="text-ink-600" aria-hidden> · </span><span className="header-state-action">{t("Sign in")}</span></span>
          <span className="sm:hidden">{t("Sign in")}</span>
        </summary>
        <div className={`absolute right-0 top-full mt-2 z-50 card p-4 shadow-xl ${device ? "w-80" : "w-72"}`}>
          {device ? (
            <>
              <p className="text-label text-ink-200 mb-2">{t("Sign in remotely")}</p>
              <div className="mb-3">
                <DeviceSignIn onSignedIn={onChange} />
              </div>
              <p className="text-label text-ink-400 mb-2">{t("Or, on the host:")}</p>
            </>
          ) : (
            <p className="text-label text-ink-200 mb-2">{t(meta.read_only ? "Sign in to your local board." : "Sign in to send messages and make decisions.")}</p>
          )}
          <p className="text-label text-ink-400 mb-2">{t("Run this command on the host, then open the link it prints in this tab.")}</p>
          <code className="num text-label text-accent break-words select-all">{cmd}</code>
        </div>
      </details>
    );
  }
  return null;
}
