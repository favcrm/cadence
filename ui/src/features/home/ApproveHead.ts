import { useCallback, useEffect, useRef, useState } from "react";
import { api, ApiError, type HostedStepUpChallenge } from "../../lib/api";
import { useTranslate } from "../../lib/localeText";
import { BURNED_STEP_UP_CHALLENGE_CODES, makeRequestId, plainRefusal } from "./MergeForm";

/** `owner/repo` and the PR number from the row's `owner/repo#N` or PR link. */
export function prTarget(pr: string | null): { repo: string; number: number } | null {
  const m = pr && (/^([\w.-]+\/[\w.-]+)#(\d+)$/.exec(pr) ?? /github\.com\/([\w.-]+\/[\w.-]+)\/pull\/(\d+)/.exec(pr));
  return m ? { repo: m[1], number: Number(m[2]) } : null;
}

export type ApprovalState = "unknown" | "missing" | "in-force" | "revoked";
export type ApproveHeadContext = { issue?: string | null; project?: string | null };
const ROLE_RESTRICTED = new Set(["member_role", "owner_required", "approver_not_allowed"]);
export type ApprovalVerdict = {
  name: string;
  agent: string | null;
  verdict: string | null;
  sha: string | null;
};

function strField(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : null;
}

function parseVerdicts(out: unknown): ApprovalVerdict[] {
  if (!out || typeof out !== "object") return [];
  const record = out as { verdicts?: unknown; precheck?: { verdicts?: unknown } };
  const raw = Array.isArray(record.verdicts)
    ? record.verdicts
    : Array.isArray(record.precheck?.verdicts)
      ? record.precheck.verdicts
      : [];
  const verdicts: ApprovalVerdict[] = [];
  for (let i = 0; i < raw.length; i += 1) {
    const item = raw[i];
    if (!item || typeof item !== "object") continue;
    const row = item as Record<string, unknown>;
    const name = strField(row.name) ?? strField(row.kind) ?? strField(row.agent) ?? `verdict-${i + 1}`;
    verdicts.push({
      name,
      agent: strField(row.agent) ?? strField(row.reviewer),
      verdict: strField(row.verdict) ?? strField(row.outcome),
      sha: strField(row.sha) ?? strField(row.head),
    });
  }
  return verdicts;
}

/**
 * The operator's approval of the exact version the drawer shows (CAD-1218),
 * and taking it back. The state is read when the drawer opens and after
 * each action, so an approval made earlier shows as approved. Records
 * only: the daemon re-reads the version and refuses a moved one, a repeat,
 * or a caller that is not the operator. The state read is a GET, so it runs
 * on a read-only board too.
 */
export function useApproveHead(pr: string | null, head: string | null, context?: ApproveHeadContext) {
  const t = useTranslate();
  const target = prTarget(pr);
  const [state, setState] = useState<ApprovalState>("unknown");
  const [approval, setApproval] = useState<string | null>(null);
  const [revocable, setRevocable] = useState(false);
  const [verdicts, setVerdicts] = useState<ApprovalVerdict[]>([]);
  const [busy, setBusy] = useState<null | "approve" | "revoke" | "send_code" | "verify_approve">(null);
  const [error, setError] = useState<string | null>(null);
  const [member, setMember] = useState(false);
  const [stepUpOpen, setStepUpOpen] = useState(false);
  const [challenge, setChallenge] = useState<HostedStepUpChallenge | null>(null);
  const [otpCode, setOtpCodeState] = useState("");
  const repo = target?.repo;
  const number = target?.number;
  const issue = context?.issue?.trim() || undefined;
  const trimmedProject = context?.project?.trim();
  const project = trimmedProject && trimmedProject !== "all" ? trimmedProject : undefined;
  const hasContext = issue !== undefined || project !== undefined;
  const targetKey = repo && number && head ? `${repo}#${number}@${head}` : "";
  const targetKeyRef = useRef(targetKey);
  targetKeyRef.current = targetKey;
  const requestIdRef = useRef<string | null>(null);
  const seqRef = useRef(0);

  const setOtpCode = useCallback((raw: string) => {
    setOtpCodeState(raw.replace(/\D+/g, "").slice(0, 6));
  }, []);

  const formatError = useCallback(
    (e: unknown) =>
      e instanceof ApiError && e.status === 409 && (!e.code || e.code === "already_approved")
        ? t("This version already has an approval on record. A fresh review makes a new version.")
        : plainRefusal(e, { project, t }),
    [project, t],
  );

  // Reset per-head state immediately when the target or head changes so an earlier
  // head's approval or restriction never leaks onto a new or empty head.
  useEffect(() => {
    seqRef.current += 1;
    setState("unknown");
    setApproval(null);
    setRevocable(false);
    setMember(false);
    setVerdicts([]);
    setBusy(null);
    setError(null);
    setStepUpOpen(false);
    setChallenge(null);
    setOtpCodeState("");
    requestIdRef.current = null;
  }, [targetKey]);

  const read = useCallback(async () => {
    if (!repo || !number || !head) return;
    const key = `${repo}#${number}@${head}`;
    const seq = ++seqRef.current;
    try {
      let out: Awaited<ReturnType<typeof api.approvalState>>;
      try {
        out = await api.approvalState(repo, number, head, hasContext ? { issue, project } : undefined);
      } catch (e) {
        if (seq !== seqRef.current || targetKeyRef.current !== key) return;
        // Tolerate boards/daemons that predate optional issue/project query params.
        if (e instanceof ApiError && e.status === 400 && hasContext) {
          out = await api.approvalState(repo, number, head);
        } else {
          throw e;
        }
      }
      if (seq !== seqRef.current || targetKeyRef.current !== key) return;
      setMember(false);
      setState(out.state);
      setApproval(out.state === "missing" ? null : (out.approval_id ?? null));
      setRevocable(out.state === "in-force" && out.board_revocable === true);
      setVerdicts(parseVerdicts(out));
    } catch (e) {
      if (seq !== seqRef.current || targetKeyRef.current !== key) return;
      setState("unknown");
      setVerdicts([]);
      // A hosted member or non-owner session may not decide: the read says so before any click.
      if (e instanceof ApiError && e.status === 403 && ROLE_RESTRICTED.has(e.check ?? e.code ?? "")) {
        setMember(true);
      }
    }
  }, [repo, number, head, hasContext, issue, project]);
  useEffect(() => {
    void read();
  }, [read]);

  const invokeApprove = useCallback(
    async (repoArg: string, numberArg: number, headArg: string, key: string, requestId?: string) => {
      const approveContext =
        hasContext || requestId
          ? {
              ...(issue ? { issue } : {}),
              ...(project ? { project } : {}),
              ...(requestId ? { request_id: requestId } : {}),
            }
          : undefined;
      try {
        return await api.approveHead(repoArg, numberArg, headArg, approveContext);
      } catch (e) {
        // Degrade safely on 400 from boards/daemons that reject fields beyond { repo, pr, head }.
        if (e instanceof ApiError && e.status === 400 && approveContext) {
          if (targetKeyRef.current !== key) return undefined;
          return await api.approveHead(repoArg, numberArg, headArg);
        }
        throw e;
      }
    },
    [hasContext, issue, project],
  );

  const sendStepUpCode = useCallback(async () => {
    if (!repo || !number || !head || busy !== null) return;
    const key = targetKey;
    setBusy("send_code");
    setError(null);
    setStepUpOpen(true);
    try {
      const next = await api.sendApprovalStepUp();
      if (targetKeyRef.current !== key) return;
      setChallenge(next);
      setOtpCodeState("");
    } catch (e) {
      // Preserve any active prior challenge on resend rate-limit / transient failure.
      if (targetKeyRef.current === key) {
        setError(formatError(e));
      }
    } finally {
      if (targetKeyRef.current === key) setBusy(null);
    }
  }, [repo, number, head, busy, targetKey, formatError]);

  const approve = async () => {
    if (!repo || !number || !head || busy !== null) return;
    const key = targetKey;
    setBusy("approve");
    setError(null);
    try {
      await invokeApprove(repo, number, head, key, requestIdRef.current ?? undefined);
      if (targetKeyRef.current !== key) return;
      setStepUpOpen(false);
      setChallenge(null);
      setOtpCodeState("");
      requestIdRef.current = null;
      await read();
    } catch (e) {
      if (targetKeyRef.current !== key) return;
      const errCode = e instanceof ApiError ? (e.check ?? e.code ?? "") : "";
      if (e instanceof ApiError && e.status === 403 && errCode === "step_up_required") {
        requestIdRef.current = requestIdRef.current ?? makeRequestId();
        setStepUpOpen(true);
        setOtpCodeState("");
        setError(null);
        try {
          const next = await api.sendApprovalStepUp();
          if (targetKeyRef.current !== key) return;
          setChallenge(next);
        } catch (sendErr) {
          if (targetKeyRef.current === key) {
            setError(formatError(sendErr));
          }
        }
        return;
      }
      if (e instanceof ApiError && (errCode === "head_moved" || errCode === "request_conflict")) {
        requestIdRef.current = null;
      }
      setError(formatError(e));
      await read();
    } finally {
      if (targetKeyRef.current === key) setBusy(null);
    }
  };

  const verifyAndApprove = useCallback(async () => {
    if (!repo || !number || !head || busy !== null) return;
    if (!challenge) {
      await sendStepUpCode();
      return;
    }
    const clean = otpCode.replace(/\D+/g, "");
    if (clean.length !== 6) {
      setError(t("Enter the 6-digit verification code."));
      return;
    }
    const key = targetKey;
    setBusy("verify_approve");
    setError(null);
    try {
      try {
        await api.verifyApprovalStepUp(challenge.challenge_id, clean);
      } catch (verifyErr) {
        if (targetKeyRef.current !== key) return;
        const code = verifyErr instanceof ApiError ? (verifyErr.check ?? verifyErr.code ?? "") : "";
        if (BURNED_STEP_UP_CHALLENGE_CODES.has(code)) {
          setChallenge(null);
          setOtpCodeState("");
        }
        setError(formatError(verifyErr));
        return;
      }
      if (targetKeyRef.current !== key) return;
      setStepUpOpen(false);
      setChallenge(null);
      setOtpCodeState("");
      const reqId = requestIdRef.current ?? makeRequestId();
      requestIdRef.current = reqId;
      try {
        await invokeApprove(repo, number, head, key, reqId);
        if (targetKeyRef.current !== key) return;
        requestIdRef.current = null;
        await read();
      } catch (approveErr) {
        if (targetKeyRef.current !== key) return;
        const code = approveErr instanceof ApiError ? (approveErr.check ?? approveErr.code ?? "") : "";
        if (code === "step_up_required") {
          setStepUpOpen(true);
        } else if (approveErr instanceof ApiError && (code === "head_moved" || code === "request_conflict")) {
          requestIdRef.current = null;
        }
        setError(formatError(approveErr));
        await read();
      }
    } finally {
      if (targetKeyRef.current === key) setBusy(null);
    }
  }, [repo, number, head, busy, challenge, otpCode, sendStepUpCode, targetKey, formatError, invokeApprove, read, t]);

  const revoke = (reason: string) => {
    const why = reason.trim();
    if (!approval) return;
    if (!why) {
      setError(t("Say why you are taking it back."));
      return;
    }
    const key = targetKey;
    setBusy("revoke");
    setError(null);
    api
      .revokeApproval(approval, why)
      .catch((e: unknown) => {
        if (targetKeyRef.current === key) setError(formatError(e));
      })
      .then(() => {
        if (targetKeyRef.current === key) return read();
        return undefined;
      })
      .finally(() => {
        if (targetKeyRef.current === key) setBusy(null);
      });
  };
  return {
    available: !!target && !!head,
    state,
    approved: state === "in-force",
    /** Only an approval a board path recorded can be taken back here. */
    revocable,
    revoked: state === "revoked",
    verdicts,
    /** A hosted member session: Approve and Publish are the owner's. */
    member,
    busy,
    error,
    stepUpOpen,
    challenge,
    otpCode,
    setOtpCode,
    clearError: () => setError(null),
    approve,
    sendStepUpCode,
    verifyAndApprove,
    revoke,
  };
}
