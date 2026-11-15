import { useCallback, useEffect, useRef, useState, type ReactElement, type ReactNode } from "react";
import { ApiError } from "../../lib/apiError";
import {
  api,
  type ApproveCardState,
  type ApproveCardView,
  type HostedStepUpChallenge,
} from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import { resources } from "../../lib/resources";
import type { IssueDetail } from "../../lib/types";
import Button from "../../ui/Button";
import SafeLink from "../../ui/SafeLink";
import { isolate } from "../decisions/stagedWrite";
import { READ_ONLY_COPY } from "./AnswerForm";
import { prTarget } from "./ApproveHead";
import { changeSize, type HomeNeed } from "./needs";

const ROLE_RESTRICTED_CODES = new Set(["member_role", "owner_required", "approver_not_allowed"]);
const UNSUPPORTED_404_CODES = new Set(["", "not_implemented", "unknown_route", "route_not_found"]);

type TranslateFn = (key: string) => string;

const GATE_REFUSAL_MESSAGES: Record<string, string> = {
  invalid_code: "That 6-digit code did not match. Check the email and try again.",
  expired_code: "That verification code has expired. Send a new code and try again.",
  challenge_not_found: "Verification challenge expired or missing. Send a new 6-digit code.",
  challenge_consumed: "That verification code was already used. Send a new code and try again.",
  too_many_attempts: "Too many verification attempts. Wait a few minutes or send a new code.",
  rate_limited: "Too many verification emails requested. Wait a moment before resending.",
  step_up_required: "Email verification is required before approving and queuing this merge.",
  email_delivery_unavailable: "Could not send the verification email right now. Try again shortly.",
  email_not_configured: "Owner verification email is not configured for this workspace.",
  no_verified_email: "Owner verification email is not configured for this workspace.",
  member_role: "Only the board's owner can approve or publish.",
  owner_required: "Only the board's owner can approve or publish.",
  approver_not_allowed: "Only the board's owner can approve or publish.",
  membership_changed: "Workspace membership changed. Refresh the gate and sign in again.",
  board_session_required: "Sign in to the hosted board again to continue.",
  origin_refused: "Request origin was refused. Reload this hosted board page and try again.",
  project_not_authorized: "This project is not authorized on the hosted merge gate.",
  project_policy_unavailable: "Project merge policy is unavailable. Check workspace policy setup and try again.",
  head_moved: "Pull request updated — a new commit was pushed.",
  request_conflict: "An approval request is already in flight or conflicted. Refresh the gate and retry.",
  gate_refused: "Merge gate refused this pull request. Review the checks above and refresh the gate.",
  already_approved: "This commit already has an approval recorded. Refresh the gate for current status.",
  approval_revoked: "This approval was already revoked.",
  already_revoked: "This approval was already revoked.",
  scope_approval_required: "This pull request needs an active ticket scope approval before delegated merge.",
  not_delegable: "This pull request touches paths that require direct owner approval.",
  project_archived: "This project is archived and no longer accepts merge approvals.",
  not_board_revocable: "This approval was not issued from the hosted board and cannot be revoked here.",
  repo_not_bound: "Repository is not bound to this hosted workspace.",
  issue_not_found: "Issue was not found on this hosted board.",
  pr_not_found: "Pull request was not found in the bound repository.",
  project_not_found: "Project was not found on this hosted workspace.",
  not_found: "The requested approval or pull request record was not found.",
  capability_unavailable: "Hosted merge service is temporarily unavailable. Try again shortly.",
  unavailable: "Hosted merge service is temporarily unavailable. Try again shortly.",
};

function apiErrorCodes(err: ApiError): string[] {
  return [err.check, err.code].filter((value): value is string => Boolean(value && value.trim()));
}

function hostedGateRefusal(err: unknown, t: TranslateFn): string {
  if (err instanceof ApiError) {
    for (const code of apiErrorCodes(err)) {
      const mapped = GATE_REFUSAL_MESSAGES[code];
      if (mapped) return t(mapped);
    }
    if (err.status === 401) return t("Sign in to the hosted board again to continue.");
    if (err.status === 429) return t("Too many requests right now. Wait a moment and try again.");
    if (err.status === 503) return t("Hosted merge service is temporarily unavailable. Try again shortly.");
    const msg = err.message.trim();
    if (msg && !/^\s*</.test(msg)) return msg;
    return t("Hosted merge request was refused.");
  }
  if (err instanceof Error) {
    const msg = err.message.trim();
    if (msg && !/^\s*</.test(msg)) return msg;
  }
  return t("Hosted merge request failed.");
}

function primaryMergeLabel(otpPromptOpen: boolean, hasChallenge: boolean, t: TranslateFn): string {
  if (!otpPromptOpen) return t("Approve & merge");
  return hasChallenge ? t("Verify & merge") : t("Send 6-digit code");
}

const STAGE_STEPS = [
  { key: "checks", label: "Checks" },
  { key: "approved", label: "Approved" },
  { key: "in_queue", label: "In queue" },
  { key: "merged", label: "Merged" },
] as const;

const STATE_BADGES: Record<ApproveCardState, { label: string; tone: "ok" | "warn" | "fail" | "accent" }> = {
  ready: { label: "Ready to merge", tone: "ok" },
  needs_1: { label: "Needs 1 approval", tone: "warn" },
  needs_n: { label: "Needs approvals", tone: "warn" },
  merging: { label: "In merge queue", tone: "accent" },
  merged: { label: "Merged", tone: "ok" },
  head_moved: { label: "Updated PR commit", tone: "warn" },
  ejected: { label: "Removed from queue", tone: "fail" },
  unavailable: { label: "Unavailable", tone: "fail" },
};

const CHECKLIST_GLYPH: Record<"pass" | "fail" | "pending" | "info", string> = {
  pass: "✓",
  fail: "✕",
  pending: "○",
  info: "·",
};

function isApproveCardView(value: unknown): value is ApproveCardView {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Partial<ApproveCardView>;
  return (
    candidate.ok === true &&
    typeof candidate.state === "string" &&
    candidate.state in STATE_BADGES &&
    Array.isArray(candidate.checklist) &&
    candidate.details !== undefined &&
    candidate.details !== null &&
    typeof candidate.details.head_sha === "string"
  );
}

function makeRequestId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return `req_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 10)}`;
}

export interface HostedMergeGateState {
  supported: boolean | null;
  loading: boolean;
  card: ApproveCardView | null;
  memberRestricted: boolean;
  loadError: string | null;
  refresh: () => Promise<ApproveCardView | null>;
}

/**
 * Fetches `GET /api/approvals/gate` for hosted workspaces and detects older
 * servers / test mocks (`supported: false`) so `DecisionPanel` can fall back
 * cleanly to the legacy approval flow without breaking non-hosted tests.
 */
export function useHostedMergeGate(params: {
  enabled: boolean;
  pr: string | null;
  head?: string | null;
  issue?: string | null;
  project?: string | null;
}): HostedMergeGateState {
  const t = useTranslate();
  const target = params.enabled ? prTarget(params.pr) : null;
  const repo = target?.repo ?? null;
  const prNumber = target?.number ?? null;
  const project = params.project && params.project !== "all" ? params.project : undefined;
  const issue = params.issue || undefined;
  const head = params.head && params.head.trim() ? params.head.trim() : undefined;

  const [supported, setSupported] = useState<boolean | null>(params.enabled && target ? null : false);
  const [loading, setLoading] = useState<boolean>(Boolean(params.enabled && target));
  const [card, setCard] = useState<ApproveCardView | null>(null);
  const [memberRestricted, setMemberRestricted] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);

  const seqRef = useRef(0);
  const mountedRef = useRef(true);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const load = useCallback(
    async (background = false): Promise<ApproveCardView | null> => {
      if (!params.enabled || !repo || prNumber === null) {
        if (mountedRef.current) {
          setSupported(false);
          setLoading(false);
          setCard(null);
          setMemberRestricted(false);
          setLoadError(null);
        }
        return null;
      }

      const seq = ++seqRef.current;
      if (!background && mountedRef.current) {
        setLoading(true);
      }

      try {
        const data = await api.approvalGate(repo, prNumber, { issue, project, head });
        if (!mountedRef.current || seq !== seqRef.current) return null;
        if (!isApproveCardView(data)) {
          setSupported(false);
          setLoading(false);
          setCard(null);
          setMemberRestricted(false);
          setLoadError(null);
          return null;
        }
        setSupported(true);
        setLoading(false);
        setCard(data);
        setMemberRestricted(false);
        setLoadError(null);
        return data;
      } catch (err) {
        if (!mountedRef.current || seq !== seqRef.current) return null;
        if (err instanceof ApiError) {
          const codes = apiErrorCodes(err);
          const isUnsupported =
            err.status === 501 ||
            (err.status === 404 &&
              !(err.check ?? "").trim() &&
              UNSUPPORTED_404_CODES.has((err.code ?? "").trim()));
          if (isUnsupported) {
            setSupported(false);
            setLoading(false);
            setCard(null);
            setMemberRestricted(false);
            setLoadError(null);
            return null;
          }
          if (err.status === 403 && codes.some((code) => ROLE_RESTRICTED_CODES.has(code))) {
            setSupported(true);
            setLoading(false);
            setCard(null);
            setMemberRestricted(true);
            setLoadError(null);
            return null;
          }
        }
        setSupported(true);
        setLoading(false);
        setMemberRestricted(false);
        setLoadError(hostedGateRefusal(err, t));
        return null;
      }
    },
    [params.enabled, repo, prNumber, issue, project, head, t],
  );

  useEffect(() => {
    void load(false);
  }, [load]);

  const refresh = useCallback(() => load(true), [load]);

  return {
    supported,
    loading,
    card,
    memberRestricted,
    loadError,
    refresh,
  };
}

function GateDetails({ card }: { card: ApproveCardView }) {
  const t = useTranslate();
  const [shown, setShown] = useState(false);
  const { details } = card;
  return (
    <div className="rv-gate-details">
      <button type="button" className="rv-dlnk" aria-expanded={shown} onClick={() => setShown((s) => !s)}>
        {t("Details")}
      </button>
      {shown && (
        <div className="rv-gate-techwrap">
          <pre className="rv-tech">
            {[
              `${card.repo}#${card.pr}`,
              details.head_sha ? `head ${details.head_sha}` : null,
              details.base_ref ? `base ${details.base_ref}${details.base_sha ? ` (${details.base_sha.slice(0, 12)})` : ""}` : null,
              details.delivery_class ? `class ${details.delivery_class}` : null,
              details.policy_digest ? `policy ${details.policy_digest}` : null,
              details.approval_id ? `approval ${details.approval_id}` : null,
              details.evidence_id ? `evidence ${details.evidence_id}` : null,
              details.approval_kind ? `mode ${details.approval_kind}` : null,
            ]
              .filter(Boolean)
              .join("\n")}
          </pre>
          {details.check_proofs.length > 0 && (
            <>
              <h5 className="rv-h">{t("Signed workflow proofs")}</h5>
              <ul className="rv-gate-proofs">
                {details.check_proofs.map((proof, i) => (
                  <li key={`${proof.context}:${i}`}>
                    <SafeLink className="lnk" href={proof.run_url}>
                      {proof.context}
                    </SafeLink>
                    <span className="num">{proof.workflow_path}</span>
                  </li>
                ))}
              </ul>
            </>
          )}
          {details.counted_verdicts.length > 0 && (
            <>
              <h5 className="rv-h">{t("Counted reviews")}</h5>
              <ul className="rv-verdicts">
                {details.counted_verdicts.map((v, i) => (
                  <li key={`${v.principal ?? v.github_login ?? "r"}:${i}`}>
                    <span>
                      <bdi>{isolate(v.github_login || v.principal || "reviewer")}</bdi> ·{" "}
                      <bdi>{isolate(`${v.kind ?? "review"}: ${v.outcome ?? "recorded"}`)}</bdi>
                    </span>
                    {v.head_sha && <small>{v.head_sha.slice(0, 12)}</small>}
                  </li>
                ))}
              </ul>
            </>
          )}
        </div>
      )}
    </div>
  );
}

function ReasonPrompt({
  label,
  placeholder,
  busy,
  onSend,
}: {
  label: string;
  placeholder: string;
  busy: boolean;
  onSend: (reason: string) => void;
}) {
  const t = useTranslate();
  const [reason, setReason] = useState("");
  const [localError, setLocalError] = useState<string | null>(null);
  return (
    <div className="rv-sendback">
      <textarea
        value={reason}
        onChange={(e) => {
          setReason(e.target.value);
          if (localError) setLocalError(null);
        }}
        onFocus={(e) => e.currentTarget.scrollIntoView?.({ block: "center" })}
        rows={3}
        placeholder={t(placeholder)}
        aria-label={t(placeholder)}
      />
      {localError && (
        <p className="rv-error m-0" role="alert">
          {localError}
        </p>
      )}
      <Button
        size="sm"
        loading={busy}
        disabled={busy}
        onClick={() => {
          const trimmed = reason.trim();
          if (!trimmed) {
            setLocalError(t("Please enter a brief reason."));
            return;
          }
          onSend(trimmed);
        }}
      >
        {t(label)}
      </Button>
    </div>
  );
}

export interface HostedGateActions {
  busy: "send_code" | "verify_merge" | "merge" | "revoke" | "send_back" | null;
  challenge: HostedStepUpChallenge | null;
  otpPromptOpen: boolean;
  otpCode: string;
  setOtpCode: (next: string) => void;
  sendBackOpen: boolean;
  setSendBackOpen: (updater: (v: boolean) => boolean) => void;
  revokeOpen: boolean;
  setRevokeOpen: (updater: (v: boolean) => boolean) => void;
  error: string | null;
  notice: string | null;
  sendCode: () => Promise<void>;
  mergeWithFreshSession: () => Promise<void>;
  verifyAndMerge: () => Promise<void>;
  revoke: (reason: string) => Promise<void>;
  sendBack: (reason: string) => Promise<void>;
}

export function useHostedGateActions(params: {
  gate: HostedMergeGateState;
  block: boolean;
  onSentBack?: (reason: string) => void;
}): HostedGateActions {
  const t = useTranslate();
  const { gate, block, onSentBack } = params;
  const card = gate.card;

  const [busy, setBusy] = useState<HostedGateActions["busy"]>(null);
  const [challenge, setChallenge] = useState<HostedStepUpChallenge | null>(null);
  const [reactiveOtpOpen, setReactiveOtpOpen] = useState(false);
  const [otpCode, setOtpCodeState] = useState("");
  const [sendBackOpen, setSendBackOpen] = useState(false);
  const [revokeOpen, setRevokeOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const attemptRef = useRef<{ key: string; requestId: string } | null>(null);
  const targetKey = card ? `${card.repo}#${card.pr}@${card.details.head_sha}` : "";

  useEffect(() => {
    setChallenge(null);
    setReactiveOtpOpen(false);
    setOtpCodeState("");
    setRevokeOpen(false);
    attemptRef.current = null;
  }, [targetKey]);

  const getRequestId = useCallback((): string => {
    if (attemptRef.current && attemptRef.current.key === targetKey) {
      return attemptRef.current.requestId;
    }
    const next = { key: targetKey, requestId: makeRequestId() };
    attemptRef.current = next;
    return next.requestId;
  }, [targetKey]);

  const setOtpCode = useCallback((raw: string) => {
    setOtpCodeState(raw.replace(/\D+/g, "").slice(0, 6));
  }, []);

  const sendCode = useCallback(async () => {
    if (block || busy) return;
    setBusy("send_code");
    setError(null);
    setNotice(null);
    try {
      const next = await api.sendApprovalStepUp();
      setChallenge(next);
      setReactiveOtpOpen(true);
      setOtpCodeState("");
    } catch (err) {
      setError(hostedGateRefusal(err, t));
    } finally {
      setBusy(null);
    }
  }, [block, busy, t]);

  const performMerge = useCallback(async () => {
    if (!card || block) return;
    const requestId = getRequestId();
    try {
      await api.approveAndMerge({
        repo: card.repo,
        pr: card.pr,
        head: card.details.head_sha,
        issue: card.issue || undefined,
        project: card.project || undefined,
        request_id: requestId,
      });
      attemptRef.current = null;
      setChallenge(null);
      setReactiveOtpOpen(false);
      setOtpCodeState("");
      setNotice(t("Approved and queued for merge."));
      void resources.overview.refresh();
      if (card.issue) void resources.issue(card.issue).refresh();
      await gate.refresh();
    } catch (err) {
      if (err instanceof ApiError && err.status === 403 && err.code === "step_up_required") {
        setReactiveOtpOpen(true);
        try {
          const next = await api.sendApprovalStepUp();
          setChallenge(next);
          setOtpCodeState("");
        } catch (sendErr) {
          setError(hostedGateRefusal(sendErr, t));
        }
        return;
      }
      if (err instanceof ApiError && err.status === 409 && err.code === "head_moved") {
        attemptRef.current = null;
        setChallenge(null);
        setReactiveOtpOpen(false);
        setOtpCodeState("");
        setError(hostedGateRefusal(err, t));
        await gate.refresh();
        return;
      }
      setError(hostedGateRefusal(err, t));
    }
  }, [card, block, getRequestId, gate, t]);

  const mergeWithFreshSession = useCallback(async () => {
    if (!card || block || busy) return;
    setBusy("merge");
    setError(null);
    setNotice(null);
    try {
      await performMerge();
    } finally {
      setBusy(null);
    }
  }, [card, block, busy, performMerge]);

  const verifyAndMerge = useCallback(async () => {
    if (!card || block || busy) return;
    if (!challenge) {
      await sendCode();
      return;
    }
    const clean = otpCode.replace(/\D+/g, "");
    if (clean.length !== 6) {
      setError(t("Enter the 6-digit verification code."));
      return;
    }
    setBusy("verify_merge");
    setError(null);
    setNotice(null);
    try {
      await api.verifyApprovalStepUp(challenge.challenge_id, clean);
      await performMerge();
    } catch (err) {
      setError(hostedGateRefusal(err, t));
    } finally {
      setBusy(null);
    }
  }, [card, block, busy, challenge, otpCode, sendCode, performMerge, t]);

  const revoke = useCallback(
    async (reason: string) => {
      const approvalId = card?.details.approval_id;
      if (!card || !approvalId || block || busy) return;
      setBusy("revoke");
      setError(null);
      setNotice(null);
      try {
        const res = await api.revokeApproval(approvalId, reason);
        setRevokeOpen(() => false);
        setNotice(
          res.outcome === "already_merged"
            ? t("Pull request already merged before revocation could dequeue it.")
            : t("Merge stopped and approval revoked."),
        );
        void resources.overview.refresh();
        if (card.issue) void resources.issue(card.issue).refresh();
        await gate.refresh();
      } catch (err) {
        setError(hostedGateRefusal(err, t));
      } finally {
        setBusy(null);
      }
    },
    [card, block, busy, gate, t],
  );

  const sendBack = useCallback(
    async (reason: string) => {
      if (!card || block || busy) return;
      setBusy("send_back");
      setError(null);
      setNotice(null);
      try {
        const res = await api.sendBackApproval({
          project: card.project,
          issue: card.issue,
          reason,
          repo: card.repo,
          pr: card.pr,
        });
        setSendBackOpen(() => false);
        void resources.overview.refresh();
        if (card.issue) void resources.issue(card.issue).refresh();
        onSentBack?.(reason);
        setNotice(
          res.outcome === "already_merged"
            ? t("Sent back, though the pull request had already merged.")
            : t("Sent back for changes."),
        );
        await gate.refresh();
      } catch (err) {
        setError(hostedGateRefusal(err, t));
      } finally {
        setBusy(null);
      }
    },
    [card, block, busy, gate, onSentBack, t],
  );

  const otpPromptOpen = Boolean(
    card && !block && card.can_approve_and_merge && (!card.step_up_fresh || reactiveOtpOpen),
  );

  return {
    busy,
    challenge,
    otpPromptOpen,
    otpCode,
    setOtpCode,
    sendBackOpen,
    setSendBackOpen,
    revokeOpen,
    setRevokeOpen,
    error,
    notice,
    sendCode,
    mergeWithFreshSession,
    verifyAndMerge,
    revoke,
    sendBack,
  };
}

export function HostedGateBody({
  gate,
  actions,
  readOnly,
  lead,
  whatChanges,
  showActions = true,
}: {
  gate: HostedMergeGateState;
  actions: HostedGateActions;
  readOnly: boolean;
  lead?: string | null;
  whatChanges?: string | null;
  showActions?: boolean;
}) {
  const t = useTranslate();
  const card = gate.card;

  if (gate.loading && !card && !gate.memberRestricted && !gate.loadError) {
    return <p className="rv-note">{t("Checking hosted merge gate…")}</p>;
  }

  if (gate.memberRestricted) {
    return (
      <div className="rv-gate" data-gate-state="member-restricted">
        <p className="rv-note">{t("Only the board's owner can approve or publish.")}</p>
      </div>
    );
  }

  if (!card) {
    return (
      <div className="rv-gate" data-gate-state="load-error">
        {gate.loadError && (
          <p className="rv-error" role="alert">
            {gate.loadError}
          </p>
        )}
        <Button size="sm" onClick={() => void gate.refresh()}>
          {t("Refresh gate")}
        </Button>
      </div>
    );
  }

  const badge = STATE_BADGES[card.state];
  const stageIdx = card.progress_stage
    ? STAGE_STEPS.findIndex((step) => step.key === card.progress_stage)
    : -1;

  return (
    <div className="rv-gate" data-gate-state={card.state}>
      <div className="rv-gate-head">
        <span className={`rv-gate-pill rv-gate-pill-${badge.tone}`} role="status">
          {t(badge.label)}
        </span>
        {card.pr_url ? (
          <SafeLink className="lnk rv-gate-prlink num" href={card.pr_url}>
            {`${card.repo}#${card.pr}`}
          </SafeLink>
        ) : (
          <span className="num rv-gate-prlink">{`${card.repo}#${card.pr}`}</span>
        )}
      </div>

      <p className="rv-gate-headline"><bdi>{isolate(card.headline)}</bdi></p>

      <ol className="rv-gate-stepper" aria-label={t("Merge gate progress")}>
        {STAGE_STEPS.map((step, idx) => {
          const done = stageIdx === STAGE_STEPS.length - 1 ? true : idx < stageIdx;
          const current = stageIdx >= 0 && stageIdx < STAGE_STEPS.length - 1 && idx === stageIdx;
          const stepState = done ? "done" : current ? "current" : "pending";
          return (
            <li
              key={step.key}
              className="rv-gate-step"
              data-stage-state={stepState}
              aria-current={current ? "step" : undefined}
            >
              <span className="rv-gate-step-dot" aria-hidden="true">
                {done ? "✓" : idx + 1}
              </span>
              <span className="rv-gate-step-label">{t(step.label)}</span>
            </li>
          );
        })}
      </ol>

      {lead && <p className="rv-lead">{lead}</p>}

      {whatChanges && (
        <>
          <h4 className="rv-h">{t("What changes")}</h4>
          <p className="rv-note">{whatChanges}</p>
        </>
      )}

      {card.checklist.length > 0 && (
        <>
          <h4 className="rv-h">{t("Merge checks")}</h4>
          <ul className="rv-gate-checklist">
            {card.checklist.map((item) => (
              <li key={item.id} className="rv-gate-check" data-check-status={item.status}>
                <span className="rv-gate-check-icon" aria-hidden="true">
                  {CHECKLIST_GLYPH[item.status]}
                </span>
                <div className="rv-gate-check-body">
                  <div className="rv-gate-check-label"><bdi>{isolate(item.label)}</bdi></div>
                  {item.summary && <div className="rv-gate-check-summary"><bdi>{isolate(item.summary)}</bdi></div>}
                </div>
              </li>
            ))}
          </ul>
        </>
      )}

      {card.details.reasons.length > 0 && (
        <>
          <h4 className="rv-h">{t("Refusal reasons")}</h4>
          <ul className="rv-gate-reasons">
            {card.details.reasons.map((reason, i) => (
              <li key={`${i}:${reason}`}>
                <bdi>{isolate(reason)}</bdi>
              </li>
            ))}
          </ul>
        </>
      )}

      {actions.otpPromptOpen && (
        <div className="rv-gate-otp" aria-label={t("Email verification")}>
          <h4 className="rv-h mt-0">{t("Email verification")}</h4>
          {!actions.challenge ? (
            <p className="rv-note m-0">
              {t("Verify with a 6-digit email code before approving and queuing this merge.")}
            </p>
          ) : (
            <>
              <p className="rv-note m-0">
                {actions.challenge.email_hint
                  ? `${t("Verification code sent to")} ${actions.challenge.email_hint}`
                  : t("Verification code sent to your owner email.")}
              </p>
              <div className="rv-gate-otp-row">
                <input
                  type="text"
                  inputMode="numeric"
                  autoComplete="one-time-code"
                  maxLength={6}
                  pattern="[0-9]{6}"
                  className="field rv-gate-otp-input num"
                  aria-label={t("6-digit verification code")}
                  placeholder="000000"
                  disabled={actions.busy !== null || readOnly}
                  value={actions.otpCode}
                  onChange={(e) => actions.setOtpCode(e.target.value)}
                  onKeyDown={(e) => {
                    if (
                      e.key === "Enter" &&
                      actions.otpCode.length === 6 &&
                      actions.busy === null &&
                      !readOnly
                    ) {
                      e.preventDefault();
                      void actions.verifyAndMerge();
                    }
                  }}
                />
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={actions.busy !== null || readOnly}
                  onClick={() => void actions.sendCode()}
                >
                  {t("Resend code")}
                </Button>
              </div>
            </>
          )}
        </div>
      )}

      {card.can_revoke && card.details.approval_id && !readOnly && (
        <div className="rv-gate-inline-action">
          <Button
            size="sm"
            disabled={actions.busy !== null}
            onClick={() => actions.setRevokeOpen((open) => !open)}
          >
            {t("Stop merge")}
          </Button>
          {actions.revokeOpen && (
            <ReasonPrompt
              label="Confirm stop merge"
              placeholder="Why are you stopping this merge?"
              busy={actions.busy === "revoke"}
              onSend={(reason) => void actions.revoke(reason)}
            />
          )}
        </div>
      )}

      {showActions && (
        <div className="rv-gate-inline-actions">
          {card.can_approve_and_merge && !readOnly && (
            <Button
              variant="primary"
              size="sm"
              loading={actions.busy === "merge" || actions.busy === "verify_merge" || actions.busy === "send_code"}
              disabled={
                actions.busy !== null ||
                (actions.otpPromptOpen && Boolean(actions.challenge) && actions.otpCode.length !== 6)
              }
              onClick={() =>
                void (actions.otpPromptOpen
                  ? actions.verifyAndMerge()
                  : actions.mergeWithFreshSession())
              }
            >
              {primaryMergeLabel(actions.otpPromptOpen, Boolean(actions.challenge), t)}
            </Button>
          )}
          {card.can_send_back && !readOnly && (
            <Button
              size="sm"
              disabled={actions.busy !== null}
              onClick={() => actions.setSendBackOpen((open) => !open)}
            >
              {t("Send back…")}
            </Button>
          )}
          <Button size="sm" variant="ghost" onClick={() => void gate.refresh()}>
            {t("Refresh gate")}
          </Button>
        </div>
      )}

      {card.can_send_back && !readOnly && actions.sendBackOpen && (
        <ReasonPrompt
          label="Send back"
          placeholder="What should change before this goes out?"
          busy={actions.busy === "send_back"}
          onSend={(reason) => void actions.sendBack(reason)}
        />
      )}

      {actions.notice && (
        <p className="rv-note" role="status">
          {actions.notice}
        </p>
      )}
      {actions.error && (
        <p className="rv-error" role="alert">
          {actions.error}
        </p>
      )}

      <GateDetails card={card} />
    </div>
  );
}

/**
 * Drawer panel replacing `MergeReview` when `GET /api/approvals/gate` is served
 * by a hosted board. Receives DecisionPanel's layout helpers via props so both
 * panels share the exact drawer shell and footer semantics.
 */
export function HostedMergeGate({
  need,
  gate,
  lead,
  block,
  index,
  total,
  onDone,
  onClose,
  onAsk,
  Shell,
  Foot,
  waiting,
}: {
  need: HomeNeed;
  gate: HostedMergeGateState;
  lead?: string | null;
  block: string | null;
  index: number;
  total: number;
  onDone: (key: string, text: string) => void;
  onClose: () => void;
  onAsk: (need: HomeNeed, lead: string) => void;
  Shell: (props: {
    need: HomeNeed;
    verb: string;
    title: string;
    subtitle: string;
    body: ReactNode;
    foot: ReactNode;
  }) => ReactElement;
  Foot: (props: {
    primary: ReactNode;
    secondary: ReactNode;
    block: string | null;
    index: number;
    total: number;
    onClose: () => void;
  }) => ReactElement;
  waiting: (need: HomeNeed, t: (s: string) => string) => string;
}) {
  const t = useTranslate();
  const action = need.action as HomeNeed["action"] & { type: "merge" };
  const actions = useHostedGateActions({
    gate,
    block: Boolean(block),
    onSentBack: () => onDone(need.key, "Sent back"),
  });

  const card = gate.card;
  const size = changeSize(need.title);
  const title = card?.title || need.shortTitle || t("Approve & merge");

  const primary = (() => {
    if (gate.loading && !card && !gate.memberRestricted && !gate.loadError) {
      return (
        <Button variant="primary" disabled loading>
          {t("Approve & merge")}
        </Button>
      );
    }
    if (gate.memberRestricted || !card) {
      return (
        <Button variant="primary" disabled>
          {t("Approve & merge")}
        </Button>
      );
    }
    if (card.state === "merging" || card.state === "merged") {
      return (
        <Button
          variant="primary"
          onClick={() => onDone(need.key, card.state === "merged" ? "Merged" : "Approved & queued")}
        >
          {t("Done")}
        </Button>
      );
    }
    if (card.state === "head_moved") {
      return (
        <Button variant="primary" onClick={() => void gate.refresh()}>
          {t("Review updated PR")}
        </Button>
      );
    }
    if (card.can_approve_and_merge) {
      const needsCodeEntry = actions.otpPromptOpen && Boolean(actions.challenge);
      return (
        <Button
          variant="primary"
          disabled={Boolean(block) || actions.busy !== null || (needsCodeEntry && actions.otpCode.length !== 6)}
          loading={actions.busy === "merge" || actions.busy === "verify_merge" || actions.busy === "send_code"}
          title={block ? t(READ_ONLY_COPY) : undefined}
          onClick={() =>
            void (actions.otpPromptOpen
              ? actions.verifyAndMerge()
              : actions.mergeWithFreshSession())
          }
        >
          {primaryMergeLabel(actions.otpPromptOpen, Boolean(actions.challenge), t)}
        </Button>
      );
    }
    return (
      <Button variant="primary" disabled title={block ? t(READ_ONLY_COPY) : undefined}>
        {t("Approve & merge")}
      </Button>
    );
  })();

  const secondary = (
    <>
      {card?.can_send_back && (
        <Button
          disabled={Boolean(block) || actions.busy !== null}
          title={block ? t(READ_ONLY_COPY) : undefined}
          onClick={() => actions.setSendBackOpen((open) => !open)}
        >
          {t("Send back…")}
        </Button>
      )}
      <Button
        disabled={Boolean(block)}
        title={block ? t(READ_ONLY_COPY) : undefined}
        onClick={() => {
          onAsk(need, `Please update ${action.issue} (${action.pr ?? action.issue}): `);
          onClose();
        }}
      >
        {t("Ask for changes…")}
      </Button>
    </>
  );

  return (
    <Shell
      need={need}
      verb="Approve & merge"
      title={title}
      subtitle={`${t("Ready")} ${waiting(need, t)}`}
      body={
        <HostedGateBody
          gate={gate}
          actions={actions}
          readOnly={Boolean(block)}
          whatChanges={size}
          lead={lead}
          showActions={false}
        />
      }
      foot={<Foot block={block} index={index} total={total} onClose={onClose} primary={primary} secondary={secondary} />}
    />
  );
}

/**
 * Mounts the live hosted merge-gate card inside the Issue PR tab when the
 * board supports `GET /api/approvals/gate`. Renders nothing on local daemons.
 */
export function HostedPrGateSection({
  detail,
  prRef,
  readOnly,
}: {
  detail: IssueDetail;
  prRef: string;
  readOnly: boolean;
}) {
  const t = useTranslate();
  const gate = useHostedMergeGate({
    enabled: Boolean(prTarget(prRef)),
    pr: prRef,
    issue: detail.id,
    project: detail.project,
  });
  const actions = useHostedGateActions({
    gate,
    block: readOnly,
  });

  if (gate.supported !== true) return null;

  return (
    <section className="card p-4 grid gap-3" aria-label={t("Hosted merge gate")}>
      <h2 className="text-cardtitle font-semibold text-ink-100 m-0">{t("Hosted merge gate")}</h2>
      <HostedGateBody gate={gate} actions={actions} readOnly={readOnly} showActions />
    </section>
  );
}
