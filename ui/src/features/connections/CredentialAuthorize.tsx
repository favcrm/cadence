import { useEffect, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import type { CredentialRequest } from "../../lib/types";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import {
  errorCopy,
  stateLabel,
  targetLabel,
  verbTitle,
} from "./credentialRequestView";
import { useProtectedEntry } from "./useProtectedEntry";
import "./credentials.css";

type Phase =
  | { kind: "loading" }
  | { kind: "ready"; request: CredentialRequest }
  | { kind: "failed"; code: string | null };

const NONCE = /^[0-9a-f]{64}$/;

/**
 * CAD-1085: the ordinary Board page that authorizes ONE handoff to the
 * pinned credential-entry origin.
 *
 * It uses the operator's real Board session (the same-origin CSRF POST is
 * the only thing that can mint the purpose/RID-bound nonce) and then
 * submits a top-level form whose BODY carries the nonce and request
 * selector. No nonce is placed in the URL/fragment, the board
 * page never sees a credential, and the entry UI is mounted by the private
 * entry-origin document that answers that POST.
 */
export default function CredentialAuthorize({ requestId }: { requestId: string }) {
  const t = useTranslate();
  const entry = useProtectedEntry();
  const [phase, setPhase] = useState<Phase>({ kind: "loading" });
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    api
      .credentialRequest(requestId)
      .then(({ request }) => {
        if (live) setPhase({ kind: "ready", request });
      })
      .catch((error: unknown) => {
        if (!live) return;
        setPhase({
          kind: "failed",
          code: error instanceof ApiError ? error.code ?? null : null,
        });
      });
    return () => {
      live = false;
    };
  }, [requestId]);

  const entryOrigin = entry.status === "ready" ? entry.entry.origin : null;
  const entryReady =
    entry.status === "ready" && entry.entry.configured && entryOrigin !== null;
  const request = phase.kind === "ready" ? phase.request : null;
  const pending = request?.state === "pending";

  useEffect(() => {
    if (!entryReady) return;
    // `no-referrer` makes a cross-origin form's Origin opaque (`null`).
    // This metadata-only authorization document must send its real origin
    // for the strict receiver, never its URL/path/selector. The private entry
    // document retains no-referrer and receives no policy override.
    const policy = document.createElement("meta");
    policy.name = "referrer";
    policy.content = "origin";
    document.head.appendChild(policy);
    return () => {
      policy.content = "no-referrer";
      policy.remove();
    };
  }, [entryReady]);

  const continueToEntry = async () => {
    if (busy || entryOrigin === null || !pending) return;
    setBusy(true);
    setFailure(null);
    try {
      const prepared = await api.credentialEntryPrepare(requestId);
      if (!NONCE.test(prepared.nonce)) {
        setFailure(errorCopy("entry_unavailable"));
        return;
      }
      // The TOP-LEVEL form is the reviewed transport: the nonce and
      // selector travel in the body only. The entry origin answers with
      // the private document that mounts the entry UI in the same tab.
      const form = document.createElement("form");
      form.method = "POST";
      form.action = `${entryOrigin}/api/credential-entry/authenticate`;
      form.style.display = "none";
      for (const [name, value] of [
        ["nonce", prepared.nonce],
        ["request_id", requestId],
      ]) {
        const input = document.createElement("input");
        input.type = "hidden";
        input.name = name;
        input.value = value;
        form.appendChild(input);
      }
      document.body.appendChild(form);
      form.submit();
    } catch (error: unknown) {
      setFailure(errorCopy(error instanceof ApiError ? error.code ?? null : null));
      setBusy(false);
    }
  };

  return (
    <main className="cred-entry">
      <section className="card cred-entry-card" data-credential-authorize="">
        <p className="cred-entry-eyebrow">{t("Cadence board")}</p>
        <h1 className="text-section font-semibold text-ink-100">{t("Authorize credential entry")}</h1>
        <p className="text-body text-ink-300 mt-2">
          {t("This authorizes one handoff to the trusted credential origin. The credential is typed there; this board never receives or stores it.")}
        </p>

        {phase.kind === "failed" ? (
          <p className="cred-entry-note text-warn" role="alert">
            {t(errorCopy(phase.code))}
            {phase.code === "operator_session_required" ||
            phase.code === "board_session_required" ? (
              <>
                {" "}
                <Link href="/login" className="lnk">
                  {t("Sign in")}
                </Link>
              </>
            ) : null}
          </p>
        ) : null}

        {request !== null ? (
          <dl className="cred-entry-meta">
            <div>
              <dt>{t("Action")}</dt>
              <dd>{t(verbTitle(request.verb))}</dd>
            </div>
            <div>
              <dt>{t("Provider")}</dt>
              <dd className="num">{request.provider}</dd>
            </div>
            <div>
              <dt>{t("Target")}</dt>
              <dd className="num">{targetLabel(request)}</dd>
            </div>
            <div>
              <dt>{t("Status")}</dt>
              <dd>{t(stateLabel(request))}</dd>
            </div>
          </dl>
        ) : null}

        {phase.kind === "loading" ? (
          <p className="cred-entry-note" role="status">
            {t("Checking this request and the trusted entry origin…")}
          </p>
        ) : null}

        {failure !== null ? (
          <p className="cred-entry-note text-warn" role="alert">
            {failure}
          </p>
        ) : null}

        {!entryReady && entry.status === "ready" ? (
          <p className="cred-entry-note text-warn" role="status">
            {t("Trusted credential entry is unsupported on this deployment. Nothing can be collected here.")}
          </p>
        ) : null}

        <div className="cred-entry-actions">
          <Button
            variant="primary"
            onClick={() => void continueToEntry()}
            loading={busy}
            disabled={busy || !entryReady || !pending}
          >
            {t("Continue on the trusted entry origin")}
          </Button>
          <Link href="/settings/connections" className="lnk">
            {t("Back to Connections")}
          </Link>
        </div>
      </section>
    </main>
  );
}
