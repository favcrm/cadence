import { Activity, useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { api, ApiError } from "../../lib/api";
import { useLocale } from "../../lib/locale";
import { allowPrivateReads, pausePrivateReads, privateSessionGeneration, resetPrivateSession } from "../../lib/privateSession";
import { applySharedSessionChange, sessionKey, setSessionKey } from "../../lib/sessionKey";
import type { Meta } from "../../lib/types";
import { navigate, useHref } from "../../lib/useLocation";
import { Logo } from "../../ui/Logo";
import Button from "../../ui/Button";
import ThemeToggle from "../../ui/ThemeToggle";
import LocalLanguage from "../../ui/LocalLanguage";
import DeviceSignIn from "./DeviceSignIn";
import Login from "./Login";
import { SIGN_IN_COMMAND } from "./gate";
import "./auth.css";
import { captureLoginNonce } from "./session";

type Status = "checking" | "signedout" | "unavailable" | "expired" | "refused" | "ready";
type BoundaryState = { status: Status; meta: Meta | null; accepted: Meta | null; generation: number; hosted: boolean };
const IDENTITY_STORE = "cadence.privateSessionIdentity";

const RETURN_STORE = "cadence.auth.returnTo";

/** Only board-local navigation survives the sign-in handoff. */
function safeReturnPath(candidate: string | null): string | null {
  if (!candidate?.startsWith("/") || candidate.startsWith("//")) return null;
  try {
    const url = new URL(candidate, location.origin);
    if (url.origin !== location.origin || url.pathname === "/login" || url.pathname.startsWith("/__platform/")) return null;
    return `${url.pathname}${url.search}${url.hash}`;
  } catch { return null; }
}

function returnDestination(): string {
  try { return safeReturnPath(sessionStorage.getItem(RETURN_STORE)) ?? "/"; } catch { return "/"; }
}
function rememberedIdentity(): string | null {
  try { return sessionStorage.getItem(IDENTITY_STORE); } catch { return null; }
}

/** Safe bootstrap only. The workspace is first mounted after read admission.
 * Activity hides a previously admitted tree and disconnects all its effects on
 * an unanswered check, retaining same-session React drafts without live reads. */
export default function AuthBoundary({ children }: {
  children: (meta: Meta, recheck: () => Promise<boolean>) => ReactNode;
}) {
  const { t, setSession } = useLocale();
  const href = useHref();
  const loginRoute = href.split(/[?#]/)[0] === "/login";
  useEffect(() => {
    const destination = safeReturnPath(href);
    if (destination) {
      try { sessionStorage.setItem(RETURN_STORE, destination); } catch { /* Same-tab return is optional when storage is unavailable. */ }
    }
  }, [href]);
  const [state, setState] = useState<BoundaryState>({ status: "checking", meta: null, accepted: null, generation: privateSessionGeneration(), hosted: false });
  const stateRef = useRef(state);
  const [loginExchanged, setLoginExchanged] = useState(false);
  const identity = useRef<string | null>(rememberedIdentity());
  const credential = useRef(sessionKey());
  const sequence = useRef(0);
  const inFlight = useRef<{ key: string | null; controller: AbortController; promise: Promise<boolean> } | null>(null);
  const title = useRef<HTMLHeadingElement>(null);
  const retryFocus = useRef(false);
  const privateFocus = useRef<HTMLElement | null>(null);
  const logoutPending = useRef(false);

  const publish = useCallback((next: BoundaryState) => {
    stateRef.current = next;
    setState(next);
  }, []);

  const discard = useCallback(() => {
    privateFocus.current = null;
    resetPrivateSession();
    identity.current = null;
    try { sessionStorage.removeItem(IDENTITY_STORE); } catch { /* Optional draft ownership marker, not a credential. */ }
    setSession(null);
  }, [setSession]);

  const recheck = useCallback((): Promise<boolean> => {
    if (logoutPending.current) return Promise.resolve(false);
    const key = sessionKey();
    if (key !== credential.current) {
      credential.current = key;
      sequence.current += 1;
      inFlight.current?.controller.abort();
      inFlight.current = null;
      discard();
      publish({ ...stateRef.current, status: "checking", accepted: null, generation: privateSessionGeneration() });
    }
    if (inFlight.current?.key === key) return inFlight.current.promise;
    const request = ++sequence.current;
    const controller = new AbortController();
    const timeout = window.setTimeout(() => controller.abort(), 8_000);
    const previous = stateRef.current;
    // Routine successful revalidation never hides/remounts the workspace.
    if (previous.status !== "ready") publish({ ...previous, status: "checking" });
    const current = () => request === sequence.current && key === sessionKey();
    const unavailable = () => {
      const focused = document.activeElement;
      if (stateRef.current.status === "ready" && focused instanceof HTMLElement && focused.closest("[data-app-shell]")) privateFocus.current = focused;
      pausePrivateReads();
      setSession(null);
      publish({ ...stateRef.current, status: "unavailable" });
    };
    const promise = api.meta(true, controller.signal).then((meta) => {
      if (!current()) return false;
      const hosted = meta.hosted === true || meta.session?.origin === "public";
      const session = meta.session;
      const sessionValid = session !== null && session !== undefined
        && typeof session.id === "string" && session.id.trim().length > 0
        && ["loopback", "tailnet", "public"].includes(session.origin)
        && Number.isFinite(session.created)
        && Number.isFinite(session.idle_expires_at)
        && Number.isFinite(session.expires_at)
        && (session.origin !== "public" || (
          typeof session.user?.sub === "string" && session.user.sub.trim().length > 0
          && (session.user.role === "operator" || session.user.role === "member")
        ));
      // A claimed sign-in without its verified identity is unanswered, not
      // authority. Explicit anonymous policy cannot rescue malformed identity.
      if (meta.signed_in === true && !sessionValid) {
        unavailable();
        return false;
      }
      const admitted = meta.signed_in === true || (!hosted && meta.anonymous_reads === true);
      if (admitted) {
        // SessionView.id is only a non-secret display prefix; stable creation
        // and expiry plus verified subject/role distinguish its observations.
        const nextIdentity = meta.signed_in === true && session
          ? JSON.stringify([hosted, session.id, session.origin, session.created, session.expires_at, session.user?.sub ?? null, session.user?.role ?? null])
          : JSON.stringify([false, "anonymous"]);
        if (identity.current !== nextIdentity) discard();
        identity.current = nextIdentity;
        try { sessionStorage.setItem(IDENTITY_STORE, nextIdentity); } catch { /* Same-page state still owns its drafts. */ }
        allowPrivateReads();
        setSession(meta);
        publish({ status: "ready", meta, accepted: meta, generation: privateSessionGeneration(), hosted });
        return true;
      }
      if (meta.signed_in !== false) {
        unavailable();
        return false;
      }
      const expired = stateRef.current.accepted !== null || identity.current !== null || key !== null || stateRef.current.status === "expired";
      discard();
      // Only a positive refusal of this exact tab credential removes it.
      if (key) {
        credential.current = null;
        setSessionKey(null);
      }
      publish({ status: expired ? "expired" : "signedout", meta, accepted: null, generation: privateSessionGeneration(), hosted });
      return false;
    }).catch((error: unknown) => {
      if (!current()) return false;
      if (error instanceof ApiError && (error.status === 403 || (error.status === 401 && error.code === "board_session_required"))) {
        const expired = stateRef.current.accepted !== null || identity.current !== null || stateRef.current.status === "expired";
        discard();
        publish({ ...stateRef.current, status: error.status === 403 ? "refused" : expired ? "expired" : "signedout", accepted: null, meta: null, hosted: true, generation: privateSessionGeneration() });
      } else unavailable();
      return false;
    }).finally(() => {
      window.clearTimeout(timeout);
      if (inFlight.current?.promise === promise) inFlight.current = null;
    });
    inFlight.current = { key, controller, promise };
    return promise;
  }, [discard, publish, setSession]);

  useEffect(() => {
    pausePrivateReads();
    void recheck();
    const changed = () => {
      const key = sessionKey();
      if (logoutPending.current && key === null) return;
      if (key !== credential.current) {
        logoutPending.current = false;
        void recheck();
      }
    };
    const ended = () => {
      sequence.current += 1;
      inFlight.current?.controller.abort();
      inFlight.current = null;
      discard();
      publish({ ...stateRef.current, status: "signedout", accepted: null, generation: privateSessionGeneration() });
    };
    const ending = () => { logoutPending.current = true; ended(); };
    const finished = () => { logoutPending.current = false; ended(); };
    const logoutUnavailable = () => {
      logoutPending.current = false;
      pausePrivateReads();
      publish({ ...stateRef.current, status: "unavailable" });
    };
    const required = () => {
      // A confirmed refusal supersedes even an older successful proof still
      // in flight. Recovery must verify anew, never join that stale probe.
      const previous = stateRef.current;
      const expired = previous.accepted !== null || identity.current !== null || sessionKey() !== null || previous.status === "expired";
      sequence.current += 1;
      inFlight.current?.controller.abort();
      inFlight.current = null;
      discard();
      if (sessionKey() !== null) {
        credential.current = null;
        setSessionKey(null);
      }
      publish({ ...previous, status: expired ? "expired" : "signedout", accepted: null, generation: privateSessionGeneration() });
    };
    const focused = () => { if (!document.hidden) void recheck(); };
    const online = () => { void recheck(); };
    const shared = (event: StorageEvent) => {
      // Another tab replaced or removed the shared key. Drop this tab's
      // fallback copy and let `changed` re-verify (or sign out) before any
      // further private work.
      if (applySharedSessionChange(event)) changed();
    };
    window.addEventListener("online", online);
    window.addEventListener("storage", shared);
    window.addEventListener("cadence:session-changed", changed);
    window.addEventListener("cadence:session-ending", ending);
    window.addEventListener("cadence:session-ended", finished);
    window.addEventListener("cadence:session-logout-unavailable", logoutUnavailable);
    window.addEventListener("cadence:session-required", required);
    window.addEventListener("focus", focused);
    document.addEventListener("visibilitychange", focused);
    const timer = window.setInterval(focused, 30_000);
    return () => {
      sequence.current += 1;
      inFlight.current?.controller.abort();
      inFlight.current = null;
      pausePrivateReads();
      window.clearInterval(timer);
      window.removeEventListener("cadence:session-changed", changed);
      window.removeEventListener("cadence:session-ending", ending);
      window.removeEventListener("cadence:session-ended", finished);
      window.removeEventListener("cadence:session-logout-unavailable", logoutUnavailable);
      window.removeEventListener("cadence:session-required", required);
      window.removeEventListener("focus", focused);
      window.removeEventListener("online", online);
      window.removeEventListener("storage", shared);
      document.removeEventListener("visibilitychange", focused);
    };
  }, [discard, publish, recheck]);

  // A local daemon restart or a brief identity outage leaves the session
  // intact (`unavailable` is not a sign-out). Retry on a short cadence so
  // the workspace comes back by itself instead of waiting for the 30 s
  // heartbeat or a manual click.
  useEffect(() => {
    if (state.status !== "unavailable") return;
    const timer = window.setInterval(() => { void recheck(); }, 3_000);
    return () => window.clearInterval(timer);
  }, [state.status, recheck]);

  useEffect(() => {
    if (state.status !== "ready" && state.status !== "checking") title.current?.focus();
    if (state.status === "ready" && (retryFocus.current || privateFocus.current)) {
      retryFocus.current = false;
      const target = privateFocus.current?.isConnected
        ? privateFocus.current
        : document.querySelector<HTMLElement>("[data-app-shell] h1, [data-app-shell] main")
          ?? document.querySelector<HTMLElement>("[data-app-shell]");
      privateFocus.current = null;
      if (target) {
        if (!target.hasAttribute("tabindex") && !target.matches("button, input, textarea, select, a[href]")) target.tabIndex = -1;
        target.focus();
      }
    }
  }, [state.status]);

  const onSignedIn = useCallback(() => {
    // Exchange is not verification: keep the public handoff until this read answers.
    void recheck().then(() => setLoginExchanged(true));
  }, [recheck]);

  useEffect(() => {
    if (loginRoute && loginExchanged && state.status === "ready") navigate(returnDestination(), { replace: true });
  }, [loginRoute, loginExchanged, state.status]);
  useEffect(() => {
    const freshLink = () => {
      if (location.pathname !== "/login" || !location.hash) return;
      captureLoginNonce();
      setLoginExchanged(false);
    };
    window.addEventListener("hashchange", freshLink);
    return () => window.removeEventListener("hashchange", freshLink);
  }, []);

  const hosted = state.hosted;
  const hostedLogin = `/__platform/login?to=${encodeURIComponent(new URL(loginRoute ? returnDestination() : safeReturnPath(href) ?? "/", location.origin).href)}`;
  const checking = state.status === "checking";
  const copy: Record<Exclude<Status, "ready">, { eyebrow: string; heading: string; text: string; icon: "lock" | "alert" | "shield" | "clock" }> = {
    checking: { eyebrow: !state.meta || hosted ? "Secure workspace" : "Local Cadence UI", heading: "Checking your session", text: !state.meta || hosted ? "Just a moment while we verify your access. Your workspace stays private until the check is complete." : "Verifying this browser’s session with the local Cadence daemon. Nothing from your workspace is shown before access is confirmed.", icon: "lock" },
    signedout: { eyebrow: hosted ? "Welcome to Cadence" : "Local Cadence UI", heading: hosted ? "Your work, behind a sign-in." : "Sign in to your local board.", text: hosted ? "Sign in with AgenticOS to open your workspace. We’ll bring you back here once your access is verified." : "On the computer running Cadence, run the command below. Open the one-use sign-in link in this browser.", icon: "lock" },
    unavailable: { eyebrow: "Verification unavailable", heading: "We couldn’t verify your session.", text: "This doesn’t mean you’ve been signed out. The connection may be interrupted. Retry to confirm your access.", icon: "alert" },
    refused: { eyebrow: hosted ? "Access required" : "Local session refused", heading: hosted ? "This workspace isn’t available to you." : "This link can’t sign you in.", text: hosted ? "Your account is signed in, but doesn’t have access to this workspace. Ask its owner for an invitation, or use another account." : "The sign-in link may have expired or already been used. Generate a fresh link on the computer running Cadence.", icon: "shield" },
    expired: { eyebrow: "Session ended", heading: "Sign in to continue.", text: "Your session has expired. Workspace content has been cleared from this view. Sign in again to reopen your work.", icon: "clock" },
  };
  const gate = copy[state.status === "ready" ? "checking" : state.status];
  const showGate = state.status !== "ready" || loginRoute;
  return (
    <div data-auth-state={state.status}>
      {showGate && <div className="auth-public">
        <header className="auth-public-header">
          <span className="auth-wordmark"><Logo size={27} /><span>cadence<small>board</small></span></span>
          <div className="auth-header-tools"><span className="auth-private-label"><GateIcon kind="lock" />{t(hosted ? "Hosted workspace" : state.meta ? "Local board" : "Private board")}</span><LocalLanguage /><ThemeToggle /></div>
        </header>
        <main className="auth-stage">
          {loginRoute && !loginExchanged ? <Login onSignedIn={onSignedIn} /> : <section className="auth-card" aria-labelledby="auth-title" aria-busy={checking}>
            <div className={`auth-state-icon${state.status === "unavailable" ? " auth-warn" : ""}`} aria-hidden="true">{checking ? <span className="auth-loader" /> : <GateIcon kind={gate.icon} />}</div>
            <p className="auth-eyebrow">{t(gate.eyebrow)}</p>
            <h1 id="auth-title" ref={title} tabIndex={-1}>{t(gate.heading)}</h1>
            <p className="auth-intro">{t(gate.text)}</p>
            {checking ? <p role="status" className="auth-session-line">{t(hosted ? "Verifying session & workspace access" : "Confirming this browser’s local session")}</p> : <div className="auth-actions">
              {state.status === "unavailable" ? <>
                <Button variant="primary" full onClick={() => { retryFocus.current = true; void recheck(); }}>{t("Retry verification")}</Button>
                {hosted ? <a className="auth-back-link" href={hostedLogin}>{t("Use another account")}</a> : <details className="auth-new-signin">
                  <summary className="auth-back-link">{t("Start a new local sign-in")}</summary>
                  <div className="auth-login-command"><code>{state.meta?.login_hint ?? SIGN_IN_COMMAND}</code><span aria-hidden="true">›_</span></div>
                  <p className="auth-quiet">{t("A fresh link is required only when this browser has no saved session. No AgenticOS account is needed for local operator login.")}</p>
                  {state.meta?.device_login === true && <DeviceSignIn onSignedIn={onSignedIn} />}
                </details>}
              </> : hosted ? <>
                <a className="btn btn-primary btn-full" href={hostedLogin}>{t(state.status === "refused" ? "Use another account" : "Continue with AgenticOS")}<span aria-hidden="true">→</span></a>
                <p className="auth-quiet auth-centered">{t("No separate Cadence account needed.")}</p>
              </> : <>
                <div className="auth-login-command"><code>{state.meta?.login_hint ?? SIGN_IN_COMMAND}</code><span aria-hidden="true">›_</span></div>
                <p className="auth-quiet">{t("A fresh link is required only when this browser has no saved session. No AgenticOS account is needed for local operator login.")}</p>
                {state.meta?.device_login === true && <DeviceSignIn onSignedIn={onSignedIn} />}
                <button className="auth-back-link" onClick={() => { void recheck(); }}>{t("Check again")}</button>
              </>}
            </div>}
            {!checking && <p className="auth-assurance"><GateIcon kind="shield" /><span>{t("Workspace content only appears after access is confirmed.")}</span></p>}
          </section>}
        </main>
        <footer className="auth-public-footer"><GateIcon kind="lock" />{t(hosted ? "Protected by AgenticOS" : "Browser-bound Cadence session")}</footer>
      </div>}
      {state.accepted !== null && !loginRoute && <Activity key={state.generation} mode={showGate ? "hidden" : "visible"}>{children(state.accepted, recheck)}</Activity>}
    </div>
  );
}

function GateIcon({ kind }: { kind: "lock" | "shield" | "alert" | "clock" }) {
  const paths: Record<typeof kind, ReactNode> = {
    lock: <><rect x="5" y="10" width="14" height="11" rx="2" /><path d="M8 10V7a4 4 0 0 1 8 0v3m-4 5v2" /></>,
    shield: <><path d="m12 3 8 3v6c0 5-8 9-8 9s-8-4-8-9V6z" /><path d="m8 12 3 3 5-6" /></>,
    alert: <><path d="m12 3 10 18H2zM12 9v4m0 4h.01" /></>,
    clock: <><circle cx="12" cy="12" r="9" /><path d="M12 7v5l3 2" /></>,
  };
  return <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{paths[kind]}</svg>;
}
