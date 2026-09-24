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
      <span
        className="chip bg-warn/10 text-warn max-w-[14rem] truncate"
        title={`Board writes need the operator's session. Run ${cmd} in your own shell on the host and open the link it prints — in this tab: each tab signs in on its own.`}
      >
        <span className="sm:hidden">{meta.tab_signed_out ? "sign in this tab" : "sign in"}</span>
        <span className="hidden sm:inline">
          {meta.tab_signed_out ? "Sign in this tab with" : "Sign in with"}&nbsp;<code className="num">{cmd}</code>
        </span>
      </span>
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
      className="chip bg-ink-800 text-ink-400 hover:text-ink-200 transition-colors"
      title={`Signed in as the operator${meta.session ? ` (session ${meta.session.id})` : ""} — click to sign out`}
    >
      <span className="hidden sm:inline">operator ·&nbsp;</span>sign out
    </button>
  );
}
