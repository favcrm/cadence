import { useCallback, useEffect, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { plainRefusal } from "./MergeForm";

/** `owner/repo` and the PR number from the row's `owner/repo#N` or PR link. */
export function prTarget(pr: string | null): { repo: string; number: number } | null {
  const m = pr && (/^([\w.-]+\/[\w.-]+)#(\d+)$/.exec(pr) ?? /github\.com\/([\w.-]+\/[\w.-]+)\/pull\/(\d+)/.exec(pr));
  return m ? { repo: m[1], number: Number(m[2]) } : null;
}

/** A refusal in plain words; an approval already on record gets its own line. */
const refusal = (e: unknown) =>
  e instanceof ApiError && e.status === 409 && e.code !== "head_moved"
    ? "This version already has an approval on record. A fresh review makes a new version."
    : plainRefusal(e);

export type ApprovalState = "unknown" | "missing" | "in-force" | "revoked";

/**
 * The operator's approval of the exact version the drawer shows (CAD-1218),
 * and taking it back. The state is read when the drawer opens and after
 * each action, so an approval made earlier shows as approved. Records
 * only: the daemon re-reads the version and refuses a moved one, a repeat,
 * or a caller that is not the operator. The state read is a GET, so it runs
 * on a read-only board too.
 */
export function useApproveHead(pr: string | null, head: string | null) {
  const target = prTarget(pr);
  const [state, setState] = useState<ApprovalState>("unknown");
  const [approval, setApproval] = useState<string | null>(null);
  const [revocable, setRevocable] = useState(false);
  const [busy, setBusy] = useState<null | "approve" | "revoke">(null);
  const [error, setError] = useState<string | null>(null);
  const [member, setMember] = useState(false);
  const repo = target?.repo;
  const number = target?.number;
  const read = useCallback(async () => {
    if (!repo || !number || !head) return;
    try {
      const out = await api.approvalState(repo, number, head);
      setState(out.state);
      setApproval(out.state === "missing" ? null : (out.approval_id ?? null));
      setRevocable(out.state === "in-force" && out.board_revocable === true);
    } catch (e) {
      setState("unknown");
      // A hosted member may not decide: the read says so before any click.
      if (e instanceof ApiError && (e.check ?? e.code) === "member_role") setMember(true);
    }
  }, [repo, number, head]);
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
    /** Only an approval a board path recorded can be taken back here. */
    revocable,
    revoked: state === "revoked",
    /** A hosted member session: Approve and Publish are the owner's. */
    member,
    busy,
    error,
    clearError: () => setError(null),
    approve,
    revoke,
  };
}
