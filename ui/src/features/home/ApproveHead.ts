import { useCallback, useEffect, useRef, useState } from "react";
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
export type ApproveHeadContext = { issue?: string | null; project?: string | null };
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
  const target = prTarget(pr);
  const [state, setState] = useState<ApprovalState>("unknown");
  const [approval, setApproval] = useState<string | null>(null);
  const [revocable, setRevocable] = useState(false);
  const [verdicts, setVerdicts] = useState<ApprovalVerdict[]>([]);
  const [busy, setBusy] = useState<null | "approve" | "revoke">(null);
  const [error, setError] = useState<string | null>(null);
  const [member, setMember] = useState(false);
  const repo = target?.repo;
  const number = target?.number;
  const issue = context?.issue?.trim() || undefined;
  const project = context?.project?.trim() || undefined;
  const hasContext = issue !== undefined || project !== undefined;
  const targetKey = repo && number && head ? `${repo}#${number}@${head}` : "";
  const targetKeyRef = useRef(targetKey);
  targetKeyRef.current = targetKey;
  const seqRef = useRef(0);

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
      // A hosted member may not decide: the read says so before any click.
      if (e instanceof ApiError && (e.check ?? e.code) === "member_role") setMember(true);
    }
  }, [repo, number, head, hasContext, issue, project]);
  useEffect(() => {
    void read();
  }, [read]);
  const run = (kind: "approve" | "revoke", call: () => Promise<unknown>) => {
    const key = targetKey;
    setBusy(kind);
    setError(null);
    call()
      .catch((e: unknown) => {
        if (targetKeyRef.current === key) setError(refusal(e));
      })
      .then(() => {
        if (targetKeyRef.current === key) return read();
        return undefined;
      })
      .finally(() => {
        if (targetKeyRef.current === key) setBusy(null);
      });
  };
  const approve = () => {
    if (!repo || !number || !head) return;
    run("approve", async () => {
      try {
        return await api.approveHead(repo, number, head, hasContext ? { issue, project } : undefined);
      } catch (e) {
        // Degrade safely on 400 from boards/daemons that reject fields beyond { repo, pr, head }.
        if (e instanceof ApiError && e.status === 400 && hasContext) {
          return await api.approveHead(repo, number, head);
        }
        throw e;
      }
    });
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
    verdicts,
    /** A hosted member session: Approve and Publish are the owner's. */
    member,
    busy,
    error,
    clearError: () => setError(null),
    approve,
    revoke,
  };
}
