import { useCallback, useEffect, useRef, useState } from "react";
import { SIGN_IN_COMMAND } from "./gate";
import { captureLoginNonce, openSession, takeLoginNonce } from "./session";
import { useLocale } from "../../lib/locale";
import { ApiError } from "../../lib/api";

type State = { kind: "working" } | { kind: "done" } | { kind: "failed"; why: string; unavailable?: boolean };

/**
 * `/login#n=<nonce>` — the page a `cadence ui login` link opens. The
 * nonce was captured and stripped from the address bar before render;
 * here it is exchanged once for the session cookie. A second link
 * opened in the same tab changes only the fragment, so no page load
 * follows: a `hashchange` listener captures, strips and exchanges it.
 */
export default function Login({ onSignedIn }: { onSignedIn: () => void }) {
  const { t } = useLocale();
  const [state, setState] = useState<State>({ kind: "working" });
  const sent = useRef(false);
  const title = useRef<HTMLHeadingElement>(null);
  const exchangeRequest = useRef(0);
  useEffect(() => { if (state.kind === "failed") title.current?.focus(); }, [state.kind]);

  const exchange = useCallback(
    (nonce: string | null) => {
      const request = ++exchangeRequest.current;
      if (!nonce) {
        setState({ kind: "failed", why: t("This page needs a sign-in link, and it has none (or it was already used here).") });
        return;
      }
      setState({ kind: "working" });
      openSession(nonce)
        .then(() => {
          if (request !== exchangeRequest.current) return;
          setState({ kind: "done" });
          onSignedIn();
        })
        .catch((e: unknown) => {
          if (request !== exchangeRequest.current) return;
          const unavailable = !(e instanceof ApiError) || e.status >= 500 || e.status === 408 || e.status === 429;
          setState({ kind: "failed", why: e instanceof Error ? e.message : String(e), unavailable });
        });
    },
    [onSignedIn, t],
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
    <section className="auth-card" data-auth-state={state.kind === "failed" ? state.unavailable ? "unavailable" : "refused" : "checking"} aria-live="polite" aria-busy={state.kind !== "failed"}>
      <div className="auth-state-icon" aria-hidden="true">{state.kind === "failed" ? "›_" : <span className="auth-loader" />}</div>
      <p className="auth-eyebrow">{t("Local Cadence UI")}</p>
      <h1 ref={title} tabIndex={-1}>{t(state.kind === "failed" ? state.unavailable ? "We couldn’t verify your session." : "This link can’t sign you in." : "Checking your session")}</h1>
      {state.kind !== "failed" && <p className="auth-intro">{t("The one-use link is exchanged for this browser’s session. Your workspace opens only after access is verified.")}</p>}
      {state.kind === "failed" && (
        <>
          <p className="auth-intro text-warn">{state.why}</p>
          <div className="auth-actions">
            <div className="auth-login-command"><code>{SIGN_IN_COMMAND}</code><span aria-hidden="true">›_</span></div>
            <p className="auth-quiet">{t("Run a fresh sign-in command on the computer running Cadence. Each link works once and belongs to this browser.")}</p>
          </div>
        </>
      )}
    </section>
  );
}
