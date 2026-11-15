import { useEffect, useState } from "react";
import { api } from "../../lib/api";
import { routinesApi } from "../agents/routinesApi";
import { useTranslate } from "../../lib/locale";
import type { Connection, CredentialRequest } from "../../lib/types";
import Button from "../../ui/Button";
import { errorCopy } from "./credentialRequestView";
import {
  chatVisibleRequests,
  entryHref,
  isResumableCompletedRequest,
  stateLabel,
  targetLabel,
  verbTitle,
} from "./credentialRequestView";
import "./credentials.css";

/**
 * CAD-1085 / CAD-2317: the trusted structured chat component + metadata-only
 * inline "Continue reply" resume after reconnect or personal account selection.
 */
export function CredentialRequestCard({
  request,
  alternateAccounts = [],
}: {
  request: CredentialRequest;
  alternateAccounts?: Connection[];
}) {
  const t = useTranslate();
  const href = entryHref(request);
  const readyToContinue = isResumableCompletedRequest(request);
  const [selectedAccount, setSelectedAccount] = useState<string>("");
  const [continuing, setContinuing] = useState(false);
  const [resumed, setResumed] = useState(false);
  const [resumeErr, setResumeErr] = useState<string | null>(null);

  const canContinue = readyToContinue || selectedAccount !== "";

  const handleContinue = async () => {
    if (continuing || !canContinue) return;
    setContinuing(true);
    setResumeErr(null);
    try {
      await routinesApi.credentialRequestContinue(
        request.request_id,
        selectedAccount ? { connection_id: selectedAccount } : undefined,
      );
      setResumed(true);
    } catch {
      setResumeErr(t("Could not continue the paused reply — check connection readiness first."));
    } finally {
      setContinuing(false);
    }
  };

  return (
    <section className="card cred-chat-card" data-credential-request={request.request_id}>
      <div className="cred-chat-head">
        <span className="text-label font-medium text-ink-100">{t(verbTitle(request.verb))}</span>
        <span className="cred-chat-state" data-tone={request.state === "pending" ? "pending" : "quiet"}>
          {resumed ? t("Reply resumed") : t(stateLabel(request))}
        </span>
      </div>
      <p className="text-micro text-ink-400 mt-1">
        {t("The operator must complete this in their own browser — nothing here can collect or show a credential.")}
      </p>
      <dl className="cred-chat-meta">
        <div>
          <dt>{t("Provider")}</dt>
          <dd className="num">{request.provider}</dd>
        </div>
        <div>
          <dt>{t("Target")}</dt>
          <dd className="num">{targetLabel(request)}</dd>
        </div>
        {request.verification_request_id ? (
          <div>
            <dt>{t("Verification request")}</dt>
            <dd className="num">{request.verification_request_id}</dd>
          </div>
        ) : null}
        {request.readiness && !request.readiness.ready ? (
          <div data-credential-blocker={request.readiness.blocker ?? "unavailable"}>
            <dt>{t("Ready")}</dt>
            <dd className="text-warn">{t(errorCopy(request.readiness.blocker))}</dd>
          </div>
        ) : null}
      </dl>

      {alternateAccounts.length > 0 && !resumed && (
        <div className="mt-2 min-w-0">
          <label className="block text-micro text-ink-400 mb-1" htmlFor={`cred-alt-${request.request_id}`}>
            {t("Or choose another connected account")}
          </label>
          <select
            id={`cred-alt-${request.request_id}`}
            className="w-full min-w-0 rounded border border-ink-700 bg-ink-875 px-2 py-1 text-label text-ink-100"
            value={selectedAccount}
            onChange={(e) => setSelectedAccount(e.target.value)}
          >
            <option value="">{t("Keep requested account")}</option>
            {alternateAccounts.map((acc) => (
              <option key={acc.id} value={acc.id}>
                {acc.account}
                {acc.policy?.is_default ? ` (${t("Default")})` : ""}
              </option>
            ))}
          </select>
        </div>
      )}

      {resumeErr && (
        <p className="text-micro text-fail mt-1" role="alert">
          {resumeErr}
        </p>
      )}

      <div className="flex flex-wrap items-center gap-2 mt-2">
        {!readyToContinue &&
          (href !== null ? (
            <Button
              variant="secondary"
              size="sm"
              href={href}
              title={t("Open the trusted entry page in its own origin")}
            >
              {t("Open secure entry")}
            </Button>
          ) : (
            <p className="text-micro text-warn" role="status">
              {t("Trusted credential entry is unsupported on this deployment — no form is served on this board.")}
            </p>
          ))}
        {canContinue && !resumed && (
          <Button
            variant="primary"
            size="sm"
            onClick={() => void handleContinue()}
            loading={continuing}
          >
            {t("Continue reply")}
          </Button>
        )}
      </div>
    </section>
  );
}

/**
 * The conversation's pending or ready-to-continue requests. The read is
 * authenticated, server-bound to this conversation and metadata only; a failure
 * shows nothing (never a fabricated card).
 */
export default function CredentialRequestList({
  conversationId,
  enabled = true,
  className,
}: {
  conversationId: string | null | undefined;
  enabled?: boolean;
  className?: string;
}) {
  const [requests, setRequests] = useState<CredentialRequest[]>([]);
  const [connections, setConnections] = useState<Connection[]>([]);
  useEffect(() => {
    if (!enabled || !conversationId) {
      setRequests([]);
      return;
    }
    let live = true;
    api
      .credentialRequests({ conversation: conversationId })
      .then((reply) => {
        if (!live) return;
        const visible = chatVisibleRequests(reply.requests);
        setRequests(visible);
        if (visible.length > 0) {
          api
            .connections()
            .then((res) => {
              if (live) setConnections(res.connections ?? []);
            })
            .catch(() => {});
        }
      })
      .catch(() => {
        // A refused/unreachable read renders no card; it never invents one.
        if (live) setRequests([]);
      });
    return () => {
      live = false;
    };
  }, [enabled, conversationId]);
  if (requests.length === 0) return null;
  return (
    <div className={className ? `cred-chat-list ${className}` : "cred-chat-list"}>
      {requests.map((request) => (
        <CredentialRequestCard
          key={request.request_id}
          request={request}
          alternateAccounts={connections.filter((c) => c.provider === request.provider)}
        />
      ))}
    </div>
  );
}

/**
 * The operator's recent requests across conversations — the reconciliation
 * list Settings shows so a CLI- or chat-created request (and a lost
 * acknowledgement) is never invisible. Read-only; a locked key still shows
 * the stored status plus its current readiness blocker.
 */
export function RecentCredentialRequestList() {
  const t = useTranslate();
  const [requests, setRequests] = useState<CredentialRequest[] | null>(null);
  useEffect(() => {
    let live = true;
    api
      .credentialRequests({ limit: 20 })
      .then((reply) => {
        if (live) setRequests(reply.requests);
      })
      .catch(() => {
        if (live) setRequests([]);
      });
    return () => {
      live = false;
    };
  }, []);
  if (requests === null || requests.length === 0) return null;
  return (
    <section className="cred-recent" aria-labelledby="cred-recent-title">
      <h3 id="cred-recent-title" className="text-label font-medium text-ink-200 mt-3">
        {t("Credential requests")}
      </h3>
      <p className="text-micro text-ink-500">
        {t("Metadata only. Completing one opens the trusted entry page; a locked custody key leaves the stored status and a readiness blocker, never a success.")}
      </p>
      <div className="cred-chat-list">
        {requests.map((request) => (
          <CredentialRequestCard key={request.request_id} request={request} />
        ))}
      </div>
    </section>
  );
}
