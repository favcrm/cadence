import type { Meta } from "../../lib/types";
import DeviceSignIn from "./DeviceSignIn";
import { SIGN_IN_COMMAND } from "./gate";
import { IconLock } from "../../ui/icons";

/**
 * The header's sign-in state: "Sign in with `cadence ui login`" when this
 * browser holds no operator session, else nothing. The signed-in identity lives in AccountMenu (CAD-1030).
 * Nothing on a read-only board (the read-only chip says it all).
 */
export default function SignIn({
  meta,
  onChange,
  access,
  onRetryAccess,
}: {
  meta: Meta | null;
  onChange: () => void;
  /** CAD-1193: how the board's access check currently reads —
   *  `"unavailable"` when a completed probe could not answer. */
  access?: "checking" | "unavailable" | null;
  /** Bounded retry of the access check (the real metadata refresh). */
  onRetryAccess?: () => void;
}) {
  // CAD-1193: `signed_in === null` is an unanswerable check —
  // unavailable, never a sign-out. A completed check that could not
  // answer says so explicitly and offers a bounded retry of the real
  // probe — never the sign-in flow, which cannot fix a stalled check.
  if (access === "unavailable" && !meta?.read_only) {
    return (
      <span className="chip bg-warn/10 text-warn" role="status">
        Access check unavailable
        {onRetryAccess && (
          <button type="button" className="lnk text-warn" onClick={onRetryAccess}>
            Retry
          </button>
        )}
      </span>
    );
  }
  if (!meta || meta.read_only || meta.signed_in === undefined || meta.signed_in === null) return null;
  if (!meta.signed_in) {
    const cmd = meta.login_hint ?? SIGN_IN_COMMAND;
    const device = meta.device_login === true;
    return (
      <details className="relative text-label">
        {/* CAD-1312: unsigned-in browsing is a neutral state, not a warning —
            the quiet pill carries the lock, "Read only" and the accent action. */}
        <summary className="header-auth-target header-state" aria-label="Read-only. Sign in to make changes">
          <IconLock size={11} />
          <span className="hidden sm:inline">Read only<span className="text-ink-600" aria-hidden> · </span><span className="header-state-action">Sign in</span></span>
          <span className="sm:hidden">Sign in</span>
        </summary>
        <div className={`absolute right-0 top-full mt-2 z-50 card p-4 shadow-xl ${device ? "w-80" : "w-72"}`}>
          {device ? (
            <>
              <p className="text-label text-ink-200 mb-2">Sign in remotely</p>
              <div className="mb-3">
                <DeviceSignIn onSignedIn={onChange} />
              </div>
              <p className="text-label text-ink-400 mb-2">Or, on the host:</p>
            </>
          ) : (
            <p className="text-label text-ink-200 mb-2">Sign in to send messages and make decisions.</p>
          )}
          <p className="text-label text-ink-400 mb-2">Run this command on the host, then open the link it prints in this tab.</p>
          <code className="num text-label text-accent break-words select-all">{cmd}</code>
        </div>
      </details>
    );
  }
  return null;
}
