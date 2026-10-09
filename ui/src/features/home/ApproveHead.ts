import { useCallback, useEffect, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { HEAD_MOVED_COPY } from "./MergeForm";

/** `owner/repo` and the PR number from the row's `owner/repo#N` or PR link. */
export function prTarget(pr: string | null): { repo: string; number: number } | null {
  const m = pr && (/^([\w.-]+\/[\w.-]+)#(\d+)$/.exec(pr) ?? /github\.com\/([\w.-]+\/[\w.-]+)\/pull\/(\d+)/.exec(pr));
  return m ? { repo: m[1], number: Number(m[2]) } : null;
}

/** A refusal in plain words; raw daemon text never reaches the card. */
const refusal = (e: unknown) => {
  if (!(e instanceof ApiError)) return "That didn't work. Try again.";
  if (e.code === "head_moved") return HEAD_MOVED_COPY;
  if (e.check === "approver_not_allowed") return "Only you can approve from here.";
  if (/allowlist/.test(e.message)) return "This device isn't allowed to approve. Ask the owner to allow it.";
  if (e.status === 409) return "This version already has an approval on record. A fresh review makes a new version.";
  if (e.status === 503) return "The board can't reach the team right now. Try again in a moment.";
  return "The approval didn't go through. Try again, or ask for help.";
};

export type ApprovalState = "unknown" | "missing" | "in-force" | "revoked";

/**
 * The operator's approval of the exact version the drawer shows (CAD-1218),
 * and taking it back. The state is read when the drawer opens and after
 * each action, so an approval made earlier shows as approved. Records
 * only: the daemon re-reads the version and refuses a moved one, a repeat,
 * or a caller that is not the operator. While `blocked` (read-only board)
 * no request is made.
 */
export function useApproveHead(pr: string | null, head: string | null, blocked: boolean) {
  const target = prTarget(pr);
  const [state, setState] = useState<ApprovalState>("unknown");
  const [approval, setApproval] = useState<string | null>(null);
  const [busy, setBusy] = useState<null | "approve" | "revoke">(null);
  const [error, setError] = useState<string | null>(null);
  const repo = target?.repo;
  const number = target?.number;
  const read = useCallback(async () => {
    if (!repo || !number || !head || blocked) return;
    try {
      const out = await api.approvalState(repo, number, head);
      setState(out.state);
      setApproval(out.state === "missing" ? null : (out.approval_id ?? null));
    } catch {
      setState("unknown");
    }
  }, [repo, number, head, blocked]);
  useEffect(() => {
    void read();
  }, [read]);
  const run = (kind: "approve" | "revoke", call: () => Promise<unknown>) => {
    setBusy(kind);
    setError(null);
    call()
      .catch((e: unknown) => setError(refusal(e)))
      .then(read)
      .finally(() => setBusy(null));
  };
  const approve = () => {
    if (!repo || !number || !head) return;
    run("approve", () => api.approveHead(repo, number, head));
  };
  const revoke = (reason: string) => {
    const why = reason.trim();
    if (!approval) return;
    if (!why) {
      setError("Say why you are taking it back.");
      return;
    }
    run("revoke", () => api.revokeApproval(approval, why));
  };
  return {
    available: !!target && !!head,
    state,
    approved: state === "in-force",
    revoked: state === "revoked",
    busy,
    error,
    approve,
    revoke,
  };
}
