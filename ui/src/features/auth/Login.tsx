import { useCallback, useEffect, useRef, useState } from "react";
import { SIGN_IN_COMMAND } from "./gate";
import { captureLoginNonce, openSession, takeLoginNonce } from "./session";

type State = { kind: "working" } | { kind: "done" } | { kind: "failed"; why: string };

/**
 * `/login#n=<nonce>` — the page a `cadence ui login` link opens. The
 * nonce was captured and stripped from the address bar before render;
 * here it is exchanged once for the session cookie. A second link
 * opened in the same tab changes only the fragment, so no page load
 * follows: a `hashchange` listener captures, strips and exchanges it.
 */
export default function Login({ onSignedIn }: { onSignedIn: () => void }) {
  const [state, setState] = useState<State>({ kind: "working" });
  const sent = useRef(false);

  const exchange = useCallback(
    (nonce: string | null) => {
      if (!nonce) {
        setState({ kind: "failed", why: "This page needs a sign-in link, and it has none (or it was already used here)." });
        return;
      }
      setState({ kind: "working" });
      openSession(nonce)
        .then(() => {
          setState({ kind: "done" });
          onSignedIn();
        })
        .catch((e) => setState({ kind: "failed", why: String(e?.message ?? e) }));
    },
    [onSignedIn],
  );

  useEffect(() => {
    if (sent.current) return;
    sent.current = true;
    exchange(takeLoginNonce());
  }, [exchange]);

  useEffect(() => {
    const onHash = () => {
      captureLoginNonce();
      const nonce = takeLoginNonce();
      if (nonce) exchange(nonce);
    };
    addEventListener("hashchange", onHash);
    return () => removeEventListener("hashchange", onHash);
  }, [exchange]);

  return (
    <section className="mx-auto max-w-lg px-4 py-10 space-y-3" aria-live="polite">
      <h1 className="text-section font-semibold text-ink-100">Sign in</h1>
      {state.kind === "working" && <p className="text-secondary text-ink-400">Signing this browser in…</p>}
      {state.kind === "done" && <p className="text-secondary text-ok">Signed in as the operator.</p>}
      {state.kind === "failed" && (
        <>
          <p className="text-secondary text-warn">{state.why}</p>
          <p className="text-secondary text-ink-400">
            Run <code className="num text-ink-200">{SIGN_IN_COMMAND}</code> in your own shell on the host and open the
            new link within two minutes. Each link works once.
          </p>
        </>
      )}
    </section>
  );
}
