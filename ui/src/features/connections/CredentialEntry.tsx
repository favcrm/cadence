import { useCallback, useEffect, useRef, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import type { CredentialRequest } from "../../lib/types";
import Button from "../../ui/Button";
import Field from "../app-shell/shared/Field";
import {
  allowsSecretInput,
  errorCopy,
  isCollectable,
  isNativeSource,
  nativeVerifyUnsupported,
  needsSmtpTransport,
  stateLabel,
  targetLabel,
  verbAction,
  verbTitle,
} from "./credentialRequestView";
import { onTrustedOrigin, useProtectedEntry } from "./useProtectedEntry";
import "./credentials.css";

type Phase =
  | { kind: "loading" }
  | { kind: "ready"; request: CredentialRequest; nonce: string }
  | { kind: "saving"; request: CredentialRequest }
  | { kind: "done"; request: CredentialRequest }
  | { kind: "unconfirmed"; checking: boolean }
  | { kind: "refused"; request: CredentialRequest | null; code: string | null };

type Transport = {
  host: string;
  port: string;
  tls_mode: "implicit" | "starttls";
  username: string;
  sender: string;
  sender_name: string;
};

const EMPTY_TRANSPORT: Transport = {
  host: "",
  port: "465",
  tls_mode: "implicit",
  username: "",
  sender: "",
  sender_name: "",
};

function codeOf(error: unknown): string | null {
  return error instanceof ApiError ? error.code ?? null : null;
}

/**
 * CAD-1085: the trusted top-level credential entry page.
 *
 * It is the fallback (and native's only) human surface for one metadata
 * request. The credential lives in this component's state for the single
 * submit call and is cleared on submit, error, cancel, unmount and when the
 * page changes to another request. Nothing is written to localStorage, the
 * composer, the transcript, analytics or a parent frame, and no value is
 * ever placed in the URL.
 *
 * The page proves the live operator session (the board forwards its real
 * session/page-key proof to the daemon; the browser never chooses a
 * binding), receives one one-use nonce and renders server metadata. Model
 * text cannot reach it: the route carries only the request id.
 */
export default function CredentialEntry({ requestId }: { requestId: string }) {
  const t = useTranslate();
  const [phase, setPhase] = useState<Phase>({ kind: "loading" });
  const [secret, setSecret] = useState("");
  const [transport, setTransport] = useState<Transport>(EMPTY_TRANSPORT);
  const [localError, setLocalError] = useState<string | null>(null);
  const generation = useRef(0);
  const secretInput = useRef<HTMLInputElement>(null);
  const secretTextarea = useRef<HTMLTextAreaElement>(null);
  // Where this deployment serves the form. The page refuses to mount a
  // credential control on any other origin (including the ordinary board),
  // even though the server already 404s the route there.
  const protectedEntry = useProtectedEntry();
  const trusted = onTrustedOrigin(protectedEntry);

  const clearSecret = useCallback(() => {
    setSecret("");
    if (secretInput.current) secretInput.current.value = "";
    if (secretTextarea.current) secretTextarea.current.value = "";
  }, []);

  const clearDraft = useCallback(() => {
    clearSecret();
    setTransport(EMPTY_TRANSPORT);
    setLocalError(null);
  }, [clearSecret]);

  // An error after submit is not proof of refusal. Reconcile only through
  // metadata: never mint another nonce or replay the credential.
  const checkStatus = useCallback(() => {
    const gen = ++generation.current;
    clearDraft();
    setPhase({ kind: "unconfirmed", checking: true });
    api.credentialRequest(requestId)
      .then((reply) => {
        if (gen === generation.current) setPhase({ kind: "done", request: reply.request });
      })
      .catch(() => {
        if (gen === generation.current) setPhase({ kind: "unconfirmed", checking: false });
      });
  }, [clearDraft, requestId]);

  const openSession = useCallback(() => {
    const gen = ++generation.current;
    setPhase({ kind: "loading" });
    clearDraft();
    api
      .credentialRequest(requestId)
      .then(async (reply) => {
        if (gen !== generation.current) return;
        if (!isCollectable(reply.request)) {
          setPhase({ kind: "done", request: reply.request });
          return;
        }
        const session = await api.credentialRequestSession(requestId);
        if (gen === generation.current) {
          setPhase({ kind: "ready", request: session.request, nonce: session.nonce });
        }
      })
      .catch((error: unknown) => {
        if (gen !== generation.current) return;
        setPhase({ kind: "refused", request: null, code: codeOf(error) });
      });
  }, [clearDraft, requestId]);

  useEffect(() => {
    if (protectedEntry.status === "loading") return;
    if (!trusted) {
      setPhase({
        kind: "refused",
        request: null,
        code:
          protectedEntry.status === "ready" &&
          protectedEntry.entry.configured &&
          protectedEntry.entry.origin !== null
            ? "wrong_entry_origin"
            : "unsupported_deployment",
      });
      return;
    }
    openSession();
    return () => {
      generation.current += 1;
      secretInput.current = null;
      secretTextarea.current = null;
    };
  }, [openSession, protectedEntry, trusted]);

  // A request identity change starts from a blank form: no typed value may
  // survive from one request to another.
  useEffect(() => {
    clearDraft();
  }, [requestId, clearDraft]);

  // Leaving the page clears the DOM control even though React unmounts it.
  useEffect(() => {
    return () => {
      if (secretInput.current) secretInput.current.value = "";
      if (secretTextarea.current) secretTextarea.current.value = "";
    };
  }, []);

  const submit = () => {
    if (phase.kind !== "ready") return;
    const { request, nonce } = phase;
    const collectable = isCollectable(request);
    if (!collectable) {
      setLocalError(errorCopy("nonce_expired"));
      return;
    }
    if (needsSmtpTransport(request)) {
      const missing =
        !transport.host.trim() ||
        !transport.username.trim() ||
        !transport.sender.trim() ||
        !/^[0-9]{1,5}$/.test(transport.port);
      if (missing) {
        setLocalError(t("Fill in the SMTP host, username, sender and port."));
        return;
      }
    }
    const wantsSecret = allowsSecretInput(request.verb);
    if (wantsSecret && secret === "") {
      setLocalError(t("Enter the credential."));
      return;
    }
    const payload: {
      nonce: string;
      secret?: string;
      transport?: {
        host: string;
        port: number;
        tls_mode: string;
        username: string;
        sender: string;
        sender_name?: string;
      };
    } = { nonce };
    if (wantsSecret) payload.secret = secret;
    if (needsSmtpTransport(request)) {
      payload.transport = {
        host: transport.host.trim(),
        port: Number(transport.port),
        tls_mode: transport.tls_mode,
        username: transport.username,
        sender: transport.sender.trim(),
        ...(transport.sender_name.trim() ? { sender_name: transport.sender_name.trim() } : {}),
      };
    }
    // Clear BEFORE the request: the one attempt owns the value, and no
    // error path can leave it displayed.
    clearSecret();
    setLocalError(null);
    setPhase({ kind: "saving", request });
    const gen = generation.current;
    api
      .credentialRequestSubmit(requestId, payload)
      .then((reply) => {
        if (gen !== generation.current) return;
        clearDraft();
        setPhase({ kind: "done", request: reply.request });
      })
      .catch(() => {
        if (gen === generation.current) checkStatus();
      });
  };

  const cancel = () => {
    if (phase.kind !== "ready" && phase.kind !== "saving") return;
    const request = phase.request;
    generation.current += 1;
    clearDraft();
    setPhase({ kind: "saving", request });
    api
      .credentialRequestCancel(requestId)
      .then((reply) => setPhase({ kind: "done", request: reply.request }))
      .catch(checkStatus);
  };

  const back =
    protectedEntry.status === "ready" ? (
      <a href={`${protectedEntry.entry.board_origin}/settings/connections`} className="lnk">
        {t("Back to Connections")}
      </a>
    ) : null;

  if (protectedEntry.status === "loading") {
    return (
      <main className="cred-entry" data-credential-entry="loading" aria-busy="true">
        <section className="card cred-entry-card" aria-live="polite">
          <p className="text-label text-ink-300">{t("Checking where this form is served…")}</p>
        </section>
      </main>
    );
  }

  if (
    protectedEntry.status === "failed" ||
    (protectedEntry.status === "ready" &&
      (protectedEntry.entry.origin === null || !protectedEntry.entry.configured))
  ) {
    return (
      <main className="cred-entry" data-credential-entry="refused">
        <section className="card cred-entry-card" role="alert">
          <h1 className="text-section font-semibold text-ink-100">{t("This request cannot open")}</h1>
          <p className="text-body text-ink-300 mt-2">{t(errorCopy("unsupported_deployment"))}</p>
          <div className="cred-entry-actions">{back}</div>
        </section>
      </main>
    );
  }

  if (
    protectedEntry.status === "ready" &&
    protectedEntry.entry.origin !== null &&
    !trusted
  ) {
    // The board SPA never renders the field; it only points at the pinned
    // origin. The server refuses the route here as well.
    const target = `${protectedEntry.entry.origin}/connections/enroll/${encodeURIComponent(requestId)}`;
    return (
      <main className="cred-entry" data-credential-entry="wrong-origin">
        <section className="card cred-entry-card" role="alert">
          <h1 className="text-section font-semibold text-ink-100">{t("Open the trusted entry page")}</h1>
          <p className="text-body text-ink-300 mt-2">{t(errorCopy("wrong_entry_origin"))}</p>
          <div className="cred-entry-actions">
            <a href={target} className="lnk">
              {t("Open secure entry")}
            </a>
            {back}
          </div>
        </section>
      </main>
    );
  }

  if (phase.kind === "loading") {
    return (
      <main className="cred-entry" data-credential-entry="loading" aria-busy="true">
        <section className="card cred-entry-card" aria-live="polite">
          <p className="text-label text-ink-300">{t("Checking this request and your session…")}</p>
        </section>
      </main>
    );
  }

  if (phase.kind === "refused") {
    return (
      <main className="cred-entry" data-credential-entry="refused">
        <section className="card cred-entry-card" role="alert">
          <h1 className="text-section font-semibold text-ink-100">{t("This request cannot open")}</h1>
          <p className="text-body text-ink-300 mt-2">{t(errorCopy(phase.code))}</p>
          <div className="cred-entry-actions">
            {phase.code === "entry_session_required" &&
            protectedEntry.status === "ready" ? (
              <p className="text-micro text-ink-400">
                {t("Continue from your Cadence board to authorize this entry.")}
                {" "}
                <a
                  href={`${protectedEntry.entry.board_origin}/connections/authorize/${encodeURIComponent(requestId)}`}
                  className="lnk"
                >
                  {t("Open the board authorization page")}
                </a>
              </p>
            ) : null}
            <Button variant="secondary" onClick={openSession}>
              {t("Check again")}
            </Button>
            {back}
          </div>
        </section>
      </main>
    );
  }

  if (phase.kind === "unconfirmed") {
    return (
      <main className="cred-entry" data-credential-entry="unknown">
        <section className="card cred-entry-card" role="status">
          <h1 className="text-section font-semibold text-ink-100">{t("Outcome not confirmed")}</h1>
          <p className="text-body text-ink-300 mt-2">{t(errorCopy("outcome_unknown"))}</p>
          <p className="cred-entry-request num">{t("Request")}: {requestId}</p>
          <div className="cred-entry-actions">
            <Button variant="secondary" onClick={checkStatus} loading={phase.checking} disabled={phase.checking}>
              {t("Check status")}
            </Button>
            {back}
          </div>
        </section>
      </main>
    );
  }

  if (phase.kind === "done") {
    const request = phase.request;
    const native = isNativeSource(request);
    // A stored native key is never a provider sign-in: only an authorized
    // model turn authenticates that source.
    const verified = request.status === "verified" && !native;
    const saved = request.state === "done" &&
      (request.status === "saved" || request.status === "verified");
    const unresolved = request.state === "unknown" || request.state === "submitting" ||
      request.state === "pending" || request.status === "unknown";
    return (
      <main className="cred-entry" data-credential-entry={unresolved ? "unknown" : "done"}>
        <section className="card cred-entry-card">
          <h1 className="text-section font-semibold text-ink-100">
            {!saved ? (unresolved ? t("Outcome not confirmed") : t(stateLabel(request)))
              : native && request.status === "verified"
              ? t("Credential saved — sign-in not verified")
              : verified
                ? t("Sign-in verified")
                : t("Credential saved")}
          </h1>
          <p className="text-body text-ink-300 mt-2">
            {!saved
              ? t(request.status === "refused" ? errorCopy(request.result_code ?? "refused") : errorCopy("outcome_unknown"))
              : native && request.status === "verified"
              ? t("The key is in the broker's encrypted source, but storing it does not authenticate the provider. Only an authorized model turn can do that, so this is not a verified sign-in and grants no source authority.")
              : verified
                ? t("The provider accepted the sign-in and the connection closed. No email was sent; this proves authentication only and grants no send or spend authority.")
                : t("The credential is in protected custody. The request is closed and cannot be submitted again.")}
          </p>
          <p className="cred-entry-target num">{targetLabel(request)}</p>
          <p className="cred-entry-request num">{t("Request")}: {requestId}</p>
          {request.verification_request_id ? (
            <p className="cred-entry-request num">
              {t("Verification request")}: {request.verification_request_id}
            </p>
          ) : null}
          {saved && request.verb === "enroll" && request.shape === "smtp" ? (
            <p className="cred-entry-note">
              {t("Saving did not verify sign-in. Return to Connections to start a separate verification request.")}
            </p>
          ) : null}
          <div className="cred-entry-actions">
            {!saved ? <Button variant="secondary" onClick={checkStatus}>{t("Check status")}</Button> : null}
            {back}
          </div>
        </section>
      </main>
    );
  }

  const request = phase.request;
  const saving = phase.kind === "saving";
  const collectable = isCollectable(request);
  const wantsSecret = allowsSecretInput(request.verb);
  return (
    <main className="cred-entry" data-credential-entry="form">
      <section className="card cred-entry-card">
        <p className="cred-entry-eyebrow">{t("Protected credential entry")}</p>
        <h1 className="text-section font-semibold text-ink-100">{t(verbTitle(request.verb))}</h1>
        <p className="text-body text-ink-300 mt-2">
          {t("This page belongs to Cadence. The value goes straight to the daemon's protected custody, is used once, and is cleared from this page immediately.")}
        </p>
        <p className="text-micro text-ink-500 mt-1">
          {t("Keep this page in the operator's separate browser profile — never a browser an agent or its browser/CDP tooling can inspect.")}
        </p>
        <dl className="cred-entry-meta">
          <div>
            <dt>{t("Provider")}</dt>
            <dd className="num">{request.provider}</dd>
          </div>
          <div>
            <dt>{t("Target")}</dt>
            <dd className="num">{targetLabel(request)}</dd>
          </div>
          {request.scopes.length > 0 ? (
            <div>
              <dt>{t("Scopes")}</dt>
              <dd className="num">{request.scopes.join(", ")}</dd>
            </div>
          ) : null}
          <div>
            <dt>{t("Status")}</dt>
            <dd>{t(stateLabel(request))}</dd>
          </div>
        </dl>

        {!collectable ? (
          <p className="cred-entry-note text-warn" role="status">
            {t("This form is no longer open. Start a new request from Connections.")}
          </p>
        ) : nativeVerifyUnsupported(request) ? (
          <div className="cred-entry-actions">
            <p className="cred-entry-note text-warn" role="alert">
              {t(errorCopy("unsupported_verification"))}
            </p>
            <Button variant="ghost" onClick={cancel}>
              {t("Cancel request")}
            </Button>
            {back}
          </div>
        ) : (
          <form
            className="cred-entry-form"
            autoComplete="off"
            onSubmit={(event) => {
              event.preventDefault();
              submit();
            }}
          >
            {needsSmtpTransport(request) ? (
              <div className="cred-entry-grid">
                <Field label={t("SMTP host")} required>
                  {(p) => (
                    <input
                      {...p}
                      className="field"
                      value={transport.host}
                      onChange={(e) => setTransport((v) => ({ ...v, host: e.target.value }))}
                      autoComplete="off"
                      spellCheck={false}
                      disabled={saving}
                    />
                  )}
                </Field>
                <Field label={t("Port")} required>
                  {(p) => (
                    <input
                      {...p}
                      className="field"
                      value={transport.port}
                      onChange={(e) => setTransport((v) => ({ ...v, port: e.target.value }))}
                      inputMode="numeric"
                      autoComplete="off"
                      disabled={saving}
                    />
                  )}
                </Field>
                <Field label={t("TLS mode")} required>
                  {(p) => (
                    <select
                      {...p}
                      className="field"
                      value={transport.tls_mode}
                      onChange={(e) =>
                        setTransport((v) => ({
                          ...v,
                          tls_mode: e.target.value === "starttls" ? "starttls" : "implicit",
                        }))
                      }
                      disabled={saving}
                    >
                      <option value="implicit">implicit</option>
                      <option value="starttls">starttls</option>
                    </select>
                  )}
                </Field>
                <Field label={t("Username")} required>
                  {(p) => (
                    <input
                      {...p}
                      className="field"
                      value={transport.username}
                      onChange={(e) => setTransport((v) => ({ ...v, username: e.target.value }))}
                      autoComplete="off"
                      spellCheck={false}
                      disabled={saving}
                    />
                  )}
                </Field>
                <Field label={t("Sender address")} required>
                  {(p) => (
                    <input
                      {...p}
                      className="field"
                      value={transport.sender}
                      onChange={(e) => setTransport((v) => ({ ...v, sender: e.target.value }))}
                      autoComplete="off"
                      spellCheck={false}
                      inputMode="email"
                      disabled={saving}
                    />
                  )}
                </Field>
                <Field label={t("Sender name")}>
                  {(p) => (
                    <input
                      {...p}
                      className="field"
                      value={transport.sender_name}
                      onChange={(e) =>
                        setTransport((v) => ({ ...v, sender_name: e.target.value }))
                      }
                      autoComplete="off"
                      disabled={saving}
                    />
                  )}
                </Field>
              </div>
            ) : null}

            {wantsSecret ? (
              <Field
                label={
                  request.shape === "smtp"
                    ? t("SMTP password")
                    : request.shape === "native-model-devin-credentials"
                      ? t("Devin credential file contents")
                      : t("API key")
                }
                required
                hint={
                  request.shape === "native-model-api-key" ||
                  request.shape === "native-model-devin-credentials"
                    ? t("The native model broker keeps this in its own encrypted source; it is not a business Connection.")
                    : t("The value is not saved in this page, your browser history or any draft.")
                }
              >
                {(p) =>
                  request.shape === "native-model-devin-credentials" ? (
                    <textarea
                      {...p}
                      ref={secretTextarea}
                      className="field"
                      rows={8}
                      value={secret}
                      onChange={(e) => setSecret(e.target.value)}
                      autoComplete="off"
                      autoCapitalize="off"
                      autoCorrect="off"
                      spellCheck={false}
                      disabled={saving}
                      data-credential-secret=""
                    />
                  ) : (
                    <input
                      {...p}
                      ref={secretInput}
                      className="field"
                      type="password"
                      value={secret}
                      onChange={(e) => setSecret(e.target.value)}
                      autoComplete="new-password"
                      autoCapitalize="off"
                      autoCorrect="off"
                      spellCheck={false}
                      disabled={saving}
                      data-credential-secret=""
                    />
                  )
                }
              </Field>
            ) : (
              <p className="cred-entry-note">
                {t("This verifies the stored sign-in only. It sends no email and grants no send or spend authority.")}
              </p>
            )}

            {localError ? (
              <p className="cred-entry-note text-warn" role="alert">
                {localError}
              </p>
            ) : null}

            <div className="cred-entry-actions">
              <Button type="submit" variant="primary" loading={saving} disabled={saving}>
                {t(verbAction(request.verb))}
              </Button>
              <Button variant="ghost" onClick={cancel} disabled={saving}>
                {t("Cancel request")}
              </Button>
              {back}
            </div>
          </form>
        )}
      </section>
    </main>
  );
}
