import { useEffect, useState } from "react";
import { api } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import type { CredentialRequest } from "../../lib/types";
import Button from "../../ui/Button";
import { errorCopy } from "./credentialRequestView";
import {
  actionableRequests,
  entryHref,
  stateLabel,
  targetLabel,
  verbTitle,
} from "./credentialRequestView";
import "./credentials.css";

/**
 * CAD-1085: the trusted structured chat component.
 *
 * It renders requests fetched from the authenticated board read for this
 * exact conversation — never from message text. A model reply that prints
 * JSON, a URL or a form cannot create one of these cards; only a durable
 * `credential_requests` row linked to the conversation does. The card holds
 * no input and no credential: opening it navigates to the top-level trusted
 * entry page.
 */
export function CredentialRequestCard({ request }: { request: CredentialRequest }) {
  const t = useTranslate();
  const href = entryHref(request);
  return (
    <section className="card cred-chat-card" data-credential-request={request.request_id}>
      <div className="cred-chat-head">
        <span className="text-label font-medium text-ink-100">{t(verbTitle(request.verb))}</span>
        <span className="cred-chat-state" data-tone={request.state === "pending" ? "pending" : "quiet"}>
          {t(stateLabel(request))}
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
      {href !== null ? (
        <Button
          variant="secondary"
          size="sm"
          href={href}
          title={t("Open the trusted entry page in its own origin")}
        >
          {t("Open secure entry")}
        </Button>
      ) : (
        <p className="text-micro text-warn mt-1" role="status">
          {t("Trusted credential entry is unsupported on this deployment — no form is served on this board.")}
        </p>
      )}
    </section>
  );
}

/**
 * The conversation's pending requests. The read is authenticated,
 * server-bound to this conversation and metadata only; a failure shows
 * nothing (never a fabricated card).
 */
export default function CredentialRequestList({
  conversationId,
  className,
}: {
  conversationId: string | null | undefined;
  className?: string;
}) {
  const [requests, setRequests] = useState<CredentialRequest[]>([]);
  useEffect(() => {
    if (!conversationId) {
      setRequests([]);
      return;
    }
    let live = true;
    api
      .credentialRequests({ conversation: conversationId })
      .then((reply) => {
        if (live) setRequests(actionableRequests(reply.requests));
      })
      .catch(() => {
        // A refused/unreachable read renders no card; it never invents one.
        if (live) setRequests([]);
      });
    return () => {
      live = false;
    };
  }, [conversationId]);
  if (requests.length === 0) return null;
  return (
    <div className={className ? `cred-chat-list ${className}` : "cred-chat-list"}>
      {requests.map((request) => (
        <CredentialRequestCard key={request.request_id} request={request} />
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
