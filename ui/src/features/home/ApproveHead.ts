import { useState } from "react";
import { api, ApiError } from "../../lib/api";
import { HEAD_MOVED_COPY } from "./MergeForm";

/** `owner/repo` and the PR number from the row's `owner/repo#N` or PR link. */
export function prTarget(pr: string | null): { repo: string; number: number } | null {
  const m = pr && (/^([\w.-]+\/[\w.-]+)#(\d+)$/.exec(pr) ?? /github\.com\/([\w.-]+\/[\w.-]+)\/pull\/(\d+)/.exec(pr));
  return m ? { repo: m[1], number: Number(m[2]) } : null;
}

/** A refusal in plain words. */
const refusal = (e: ApiError) => {
  if (e.code === "head_moved") return HEAD_MOVED_COPY;
  if (e.check === "approver_not_allowed") return "Only you can approve from here.";
  if (/allowlist/.test(e.message)) return "This device isn't allowed to approve. Ask the owner to allow it.";
  if (e.status === 409 && /already/.test(e.message)) {
    return "This version already has an approval on record. A fresh review makes a new version.";
  }
  return e.message ?? String(e);
};

/**
 * The operator's approval of the exact version the drawer shows (CAD-1218),
 * and taking it back. Records only: the daemon re-reads the version and
 * refuses a moved one, a repeat, or a caller that is not the operator.
 */
export function useApproveHead(pr: string | null, head: string | null) {
  const target = prTarget(pr);
  const [approval, setApproval] = useState<string | null>(null);
  const [busy, setBusy] = useState<null | "approve" | "revoke">(null);
  const [error, setError] = useState<string | null>(null);
  const [revoked, setRevoked] = useState(false);
  const run = (kind: "approve" | "revoke", call: () => Promise<void>) => {
    setBusy(kind);
    setError(null);
    call()
      .catch((e: ApiError) => setError(refusal(e)))
      .finally(() => setBusy(null));
  };
  const approve = () => {
    if (!target || !head) return;
    run("approve", async () => {
      const out = await api.approveHead(target.repo, target.number, head);
      setApproval(out.approval_id);
      setRevoked(false);
    });
  };
  const revoke = (reason: string) => {
    const why = reason.trim();
    if (!approval) return;
    if (!why) {
      setError("Say why you are taking it back.");
      return;
    }
    run("revoke", async () => {
      await api.revokeApproval(approval, why);
      setApproval(null);
      setRevoked(true);
    });
  };
  return { available: !!target && !!head, approved: approval !== null, revoked, busy, error, approve, revoke };
}
