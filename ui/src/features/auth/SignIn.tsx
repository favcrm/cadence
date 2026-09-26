import { useState } from "react";
import type { Meta } from "../../lib/types";
import { SIGN_IN_COMMAND } from "./gate";
import { closeSession } from "./session";

/**
 * The header's sign-in state: "Sign in with `cadence ui login`" when this
 * browser holds no operator session, else a signed-in chip that signs out.
 * Nothing on a read-only board (the read-only chip says it all).
 */
export default function SignIn({ meta, onChange }: { meta: Meta | null; onChange: () => void }) {
  const [busy, setBusy] = useState(false);
  if (!meta || meta.read_only || meta.signed_in === undefined) return null;
  if (!meta.signed_in) {
    const cmd = meta.login_hint ?? SIGN_IN_COMMAND;
    return (
      <details className="relative text-label">
        <summary className="header-auth-target chip bg-warn/10 text-warn cursor-pointer" aria-label="Read-only. Sign in to make changes"><span className="hidden sm:inline">Read-only · </span>Sign in</summary>
        <div className="absolute right-0 top-full mt-2 z-50 card p-4 w-72 shadow-xl">
          <p className="text-label text-ink-200 mb-2">Sign in to send messages and make decisions.</p>
          <p className="text-label text-ink-400 mb-2">Run this command on the host, then open the link it prints in this tab.</p>
          <code className="num text-label text-accent break-words select-all">{cmd}</code>
        </div>
      </details>
    );
  }
  return (
    <button
      type="button"
      disabled={busy}
      onClick={() => {
        setBusy(true);
        closeSession()
          .catch(() => undefined)
          .finally(() => {
            setBusy(false);
            onChange();
          });
      }}
      className="header-auth-target chip bg-ink-800 text-ink-400 hover:text-ink-200 transition-colors"
      title={`Signed in as the operator${meta.session ? ` (session ${meta.session.id})` : ""} — click to sign out`}
    >
      <span className="hidden sm:inline">operator ·&nbsp;</span>sign out
    </button>
  );
}
