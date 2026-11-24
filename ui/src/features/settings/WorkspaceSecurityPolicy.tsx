import { useCallback, useEffect, useId, useRef, useState, type FormEvent } from "react";
import { api } from "../../lib/api";
import { ApiError } from "../../lib/apiError";
import { useTranslate } from "../../lib/locale";
import { onPrivateSessionReset, privateFetch, withPrivateOperation } from "../../lib/privateSession";
import { sessionHeaders } from "../../lib/sessionKey";
import Button from "../../ui/Button";
import Switch from "../../ui/Switch";

type PendingMember = { userId: string; name: string; email: string };
type PolicyDetail = {
  policy: { requireTwoFactor: boolean; revision: number; updatedAt: string | null };
  readiness: { totalMembers: number; enrolledMembers: number; pendingMembers: PendingMember[] };
};
type Snapshot = { detail: PolicyDetail; etag: string };

function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function parseSnapshot(body: unknown, etag: string | null): Snapshot {
  if (!record(body) || body.ok !== true || !record(body.data) || !etag || !/^"\d+"$/.test(etag)) throw new Error("Invalid policy receipt");
  const { policy, readiness } = body.data;
  if (!record(policy) || typeof policy.requireTwoFactor !== "boolean" || !Number.isSafeInteger(policy.revision) || (policy.revision as number) < 0 || etag !== `"${policy.revision}"` || !(typeof policy.updatedAt === "string" || policy.updatedAt === null) ||
      !record(readiness) || !Number.isSafeInteger(readiness.totalMembers) || (readiness.totalMembers as number) < 0 || !Number.isSafeInteger(readiness.enrolledMembers) || (readiness.enrolledMembers as number) < 0 || (readiness.enrolledMembers as number) > (readiness.totalMembers as number) || !Array.isArray(readiness.pendingMembers)) throw new Error("Invalid policy receipt");
  const pendingMembers = readiness.pendingMembers.map((member: unknown): PendingMember => {
    if (!record(member) || typeof member.userId !== "string" || !member.userId || typeof member.name !== "string" || typeof member.email !== "string" || !member.email || (member.role !== "owner" && member.role !== "member") || typeof member.twoFactorEnabled !== "boolean") throw new Error("Invalid policy member");
    return { userId: member.userId, name: member.name, email: member.email };
  });
  return { etag, detail: { policy: { requireTwoFactor: policy.requireTwoFactor, revision: policy.revision as number, updatedAt: policy.updatedAt }, readiness: { totalMembers: readiness.totalMembers as number, enrolledMembers: readiness.enrolledMembers as number, pendingMembers } } };
}

async function policyRequest(signal: AbortSignal, write?: { requireTwoFactor: boolean; code: string; etag: string }): Promise<Snapshot> {
  return withPrivateOperation(async (operation) => {
    const response = await privateFetch("/api/account/security/workspace-policy", write ? {
      method: "PATCH",
      headers: { ...sessionHeaders(), "Content-Type": "application/json", "X-Cadence-Board": "1", "If-Match": write.etag },
      body: JSON.stringify({ requireTwoFactor: write.requireTwoFactor, code: write.code }),
    } : { headers: sessionHeaders() }, operation);
    const body: unknown = await response.json().catch(() => null);
    operation.assertCurrent();
    if (!response.ok) {
      const code = record(body) && record(body.error) && typeof body.error.code === "string" ? body.error.code : undefined;
      throw new ApiError("Workspace policy request failed", response.status, { code });
    }
    return parseSnapshot(body, response.headers.get("etag"));
  }, signal);
}

export default function WorkspaceSecurityPolicy({ onSaved }: { onSaved: (signal: AbortSignal) => Promise<void> }) {
  const t = useTranslate();
  const id = useId();
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [desired, setDesired] = useState(false);
  const [code, setCode] = useState("");
  const [emailCode, setEmailCode] = useState("");
  const [challenge, setChallenge] = useState<string | null>(null);
  const [stepUp, setStepUp] = useState(false);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const controller = useRef<AbortController | null>(null);
  const sequence = useRef(0);
  const reset = useCallback(() => {
    sequence.current++;
    controller.current?.abort();
    setSnapshot(null); setCode(""); setEmailCode(""); setChallenge(null); setStepUp(false); setBusy(false); setNotice(null); setError(null); setLoading(false);
  }, []);
  const load = useCallback(async () => {
    const serial = ++sequence.current;
    controller.current?.abort();
    const next = new AbortController(); controller.current = next;
    setSnapshot(null); setLoading(true); setError(null); setNotice(null); setCode(""); setEmailCode(""); setChallenge(null); setStepUp(false);
    try {
      const value = await policyRequest(next.signal);
      if (serial !== sequence.current || next.signal.aborted) return;
      setSnapshot(value); setDesired(value.detail.policy.requireTwoFactor);
    } catch {
      if (serial === sequence.current && !next.signal.aborted) setError("Could not load workspace security policy.");
    } finally {
      if (serial === sequence.current && !next.signal.aborted) setLoading(false);
    }
  }, []);
  useEffect(() => {
    void load();
    const unsubscribe = onPrivateSessionReset(reset);
    return () => { unsubscribe(); sequence.current++; controller.current?.abort(); };
  }, [load, reset]);

  const save = async (event: FormEvent) => {
    event.preventDefault();
    if (!snapshot || busy || !controller.current || desired === snapshot.detail.policy.requireTwoFactor) return;
    if (!/^\d{6}$/.test(code)) { setError("Enter the 6-digit verification code."); return; }
    const serial = sequence.current, signal = controller.current.signal;
    setBusy(true); setError(null); setNotice(null);
    try {
      const result = await policyRequest(signal, { requireTwoFactor: desired, code, etag: snapshot.etag });
      if (serial !== sequence.current || signal.aborted) return;
      if (result.detail.policy.requireTwoFactor !== desired) throw new Error("Unexpected policy receipt");
      setSnapshot(result); setDesired(result.detail.policy.requireTwoFactor); setCode(""); setEmailCode(""); setChallenge(null); setStepUp(false); setNotice("Workspace security policy saved.");
      void onSaved(signal);
    } catch (failure) {
      if (serial !== sequence.current || signal.aborted) return;
      if (failure instanceof ApiError && failure.code === "step_up_required") {
        setStepUp(true); setError(null);
      } else if (failure instanceof ApiError && failure.code === "owner_two_factor_required") {
        setCode(""); setError("Enable two-factor authentication on your own account first.");
      } else if (failure instanceof ApiError && (failure.status === 409 || failure.status === 412)) {
        setSnapshot(null); setCode(""); setError("The policy changed. Refresh and try again.");
      } else if (failure instanceof ApiError && failure.code === "invalid_totp_code") {
        setCode(""); setError("That verification code didn’t match. Try the latest 6-digit code.");
      } else {
        setSnapshot(null); setCode(""); setError("Could not save workspace security policy. Refresh and check the current policy before trying again.");
      }
    } finally { if (serial === sequence.current && !signal.aborted) setBusy(false); }
  };

  const sendEmail = async () => {
    if (busy || !controller.current) return;
    const serial = sequence.current, signal = controller.current.signal;
    setBusy(true); setError(null);
    try {
      const result = await withPrivateOperation(async (operation) => { const value = await api.sendApprovalStepUp(); operation.assertCurrent(); return value; }, signal);
      if (serial !== sequence.current || signal.aborted) return;
      if (!result || typeof result.challenge_id !== "string" || !result.challenge_id) throw new Error("Invalid challenge");
      setChallenge(result.challenge_id); setEmailCode("");
    } catch { if (serial === sequence.current && !signal.aborted) setError("Could not send a verification code. Try again."); }
    finally { if (serial === sequence.current && !signal.aborted) setBusy(false); }
  };
  const verifyEmail = async (event: FormEvent) => {
    event.preventDefault();
    if (!challenge || busy || !controller.current) return;
    if (!/^\d{6}$/.test(emailCode)) { setError("Enter the 6-digit verification code."); return; }
    const serial = sequence.current, signal = controller.current.signal;
    setBusy(true); setError(null);
    try {
      const receipt = await withPrivateOperation(async (operation) => { const value = await api.verifyApprovalStepUp(challenge, emailCode); operation.assertCurrent(); return value; }, signal);
      if (serial !== sequence.current || signal.aborted) return;
      if (!receipt || receipt.ok !== true) throw new Error("Invalid verification receipt");
      setStepUp(false); setChallenge(null); setEmailCode(""); setCode(""); setNotice("Email verified. Enter a current authenticator code and save the policy.");
    } catch { if (serial === sequence.current && !signal.aborted) setError("Could not verify your email. Try again."); }
    finally { if (serial === sequence.current && !signal.aborted) setBusy(false); }
  };

  return <section className="security-section" aria-labelledby={`${id}-title`}>
    <h2 id={`${id}-title`} className="text-cardtitle font-medium text-ink-100">{t("Workspace two-factor policy")}</h2>
    {loading && <p role="status" className="mt-2 text-label text-ink-400">{t("Loading security settings…")}</p>}
    {error && <p role="alert" className="security-error mt-2">{t(error)}</p>}
    {notice && <p role="status" className="mt-2 text-label text-ink-400">{t(notice)}</p>}
    {!loading && !snapshot && <Button onClick={() => void load()}>{t("Retry")}</Button>}
    {snapshot && <>
      <div className="security-row-head mt-3">
        <span className="text-label text-ink-300">{t("Require two-factor authentication for all workspace members")}</span>
        <Switch checked={desired} onToggle={() => { setDesired(!desired); setError(null); setNotice(null); setStepUp(false); setChallenge(null); setEmailCode(""); }} label={t("Require two-factor authentication for all workspace members")} disabled={busy} />
      </div>
      <p className="mt-2 text-label text-ink-400">{t("Enrolled: {enrolled} of {total}").replace("{enrolled}", String(snapshot.detail.readiness.enrolledMembers)).replace("{total}", String(snapshot.detail.readiness.totalMembers))}</p>
      {snapshot.detail.readiness.pendingMembers.length > 0 && <div className="mt-2 text-label text-ink-400"><p>{t("Members who have not enrolled")}</p><ul>{snapshot.detail.readiness.pendingMembers.map(member => <li key={member.userId}>{member.name ? `${member.name} · ${member.email}` : member.email}</li>)}</ul></div>}
      {desired !== snapshot.detail.policy.requireTwoFactor && !stepUp && <form className="security-panel" onSubmit={save} aria-labelledby={`${id}-title`}>
        {desired && snapshot.detail.readiness.pendingMembers.length > 0 && <p className="text-label text-ink-400">{t("Members who have not enrolled will need to set up two-factor authentication to access this workspace.")}</p>}
        <div className="security-field"><label htmlFor={`${id}-code`} className="text-label text-ink-300">{t("Authenticator code")}</label><input id={`${id}-code`} className="field w-full" type="text" inputMode="numeric" autoComplete="one-time-code" value={code} onChange={e => setCode(e.target.value)} maxLength={6} disabled={busy} /></div>
        <div className="security-actions"><Button type="submit" variant="primary" loading={busy}>{t("Save policy")}</Button></div>
      </form>}
      {stepUp && <div className="security-panel"><p className="text-label text-ink-400">{t("Verify your email before changing this policy.")}</p>
        {!challenge ? <Button onClick={() => void sendEmail()} loading={busy}>{t("Send verification code")}</Button> : <form onSubmit={verifyEmail} aria-labelledby={`${id}-title`}><div className="security-field"><label htmlFor={`${id}-email-code`} className="text-label text-ink-300">{t("Email verification code")}</label><input id={`${id}-email-code`} className="field w-full" type="text" inputMode="numeric" autoComplete="one-time-code" value={emailCode} onChange={e => setEmailCode(e.target.value)} maxLength={6} disabled={busy} /></div><div className="security-actions"><Button type="submit" variant="primary" loading={busy}>{t("Verify email")}</Button><Button onClick={() => void sendEmail()} disabled={busy}>{t("Send verification code")}</Button></div></form>}
      </div>}
    </>}
  </section>;
}
