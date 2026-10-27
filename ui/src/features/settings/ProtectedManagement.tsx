import { useMemo, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import Button from "../../ui/Button";
import { entryHref } from "../connections/credentialRequestView";
import type { CredentialRequest } from "../../lib/types";

/**
 * CAD-1085 protected custody and Connection management.
 *
 * Every control runs the SAME real flow the daemon exposes: one one-use
 * management nonce bound to this operator session and the exact action
 * parameters, then the typed action, then a metadata-only status. No control
 * invents readiness, none is enabled by a local flag, and no credential value
 * ever passes through this panel (credentials are typed on the isolated entry
 * page). Structured documents (manifest/policy) are still server-validated:
 * an invalid one changes nothing.
 */

type Attempt = {
  id: string;
  state: string | null;
  code: string | null;
  action: string | null;
};

const MANAGEMENT_ERROR: Record<string, string> = {
  entry_session_required:
    "Sign in as the operator in this browser, then retry. Nothing was changed.",
  request_not_ready:
    "Protected management is not ready on this daemon. Nothing was changed.",
  already_submitted: "That attempt was already used. Start a new one.",
  nonce_expired: "That attempt expired. Start a new one.",
  request_changed: "The protected state changed. Start a new attempt.",
  invalid_request: "That input was not accepted. Nothing was changed.",
  outcome_unknown:
    "The outcome is unknown. Do not retry; check the attempt status to reconcile.",
  custody_unavailable:
    "This daemon has no protected custody profile configured. Nothing was changed.",
  custody_locked: "Custody is locked. Unlock it first.",
  state_conflict: "That state change conflicts with the current authority.",
  stale_revision: "The revision changed. Reload and retry.",
  placement_changed: "The protected workspace changed. Reload and retry.",
  runtime_revoked: "The protected profile is fenced or revoked.",
  runtime_unqualified: "The protected runtime is not qualified right now.",
  not_found: "That selector is unavailable.",
  rate_limit: "The metadata/read rate limit is reached. Try again shortly.",
  refused: "The daemon refused this action. Nothing more was changed.",
};

function errorCopy(error: unknown): string {
  if (error instanceof ApiError && error.code && MANAGEMENT_ERROR[error.code]) {
    return MANAGEMENT_ERROR[error.code];
  }
  return "The action could not be completed. Check the attempt status before retrying.";
}

/** Exact decimal dollars -> integer USD micros, without float rounding. */
function dollarsToMicros(text: string): string | null {
  const value = text.trim();
  if (!/^(0|[1-9][0-9]*)(\.[0-9]{1,6})?$/.test(value)) return null;
  const [whole, fraction = ""] = value.split(".");
  const micros = `${whole}${fraction.padEnd(6, "0")}`.replace(/^0+(?=\d)/, "");
  if (!/^[0-9]{1,19}$/.test(micros)) return null;
  return micros;
}

function counter(text: string): string | null {
  const value = text.trim();
  return /^(0|[1-9][0-9]{0,18})$/.test(value) ? value : null;
}

function list(text: string): string[] {
  return text
    .split(",")
    .map((item) => item.trim())
    .filter((item) => item.length > 0);
}

type LimitRowProps = {
  label: string;
  values: string[];
  onChange: (index: number, value: string) => void;
};

const LIMIT_FIELDS = ["req/60s", "unresolved", "$/day", "$ lifetime", "$ per call"];

function LimitRow({ label, values, onChange }: LimitRowProps) {
  return (
    <div className="grid grid-cols-2 sm:grid-cols-6 gap-2 items-end">
      <span className="text-label text-ink-400 col-span-2 sm:col-span-1">{label}</span>
      {values.map((value, index) => (
        <label key={LIMIT_FIELDS[index] ?? index} className="text-micro text-ink-500 flex flex-col gap-1">
          {LIMIT_FIELDS[index]}
          <input
            className="field"
            inputMode="decimal"
            value={value}
            onChange={(event) => onChange(index, event.target.value)}
            aria-label={`${label} ${LIMIT_FIELDS[index]}`}
          />
        </label>
      ))}
    </div>
  );
}

const PRINCIPAL_DEFAULT = ["10", "2", "5", "50", "1"];
const ACCOUNT_DEFAULT = ["30", "4", "20", "100", "2"];
const WORKSPACE_DEFAULT = ["60", "8", "50", "250", "5"];

export default function ProtectedManagement({ canWrite }: { canWrite: boolean }) {
  const t = useTranslate();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState<Attempt | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const [enabled, setEnabled] = useState(true);
  const [principal, setPrincipal] = useState(PRINCIPAL_DEFAULT);
  const [account, setAccount] = useState(ACCOUNT_DEFAULT);
  const [workspace, setWorkspace] = useState(WORKSPACE_DEFAULT);

  const [manifestId, setManifestId] = useState("");
  const [manifestRevision, setManifestRevision] = useState("1");
  const [manifestPolicyDigest, setManifestPolicyDigest] = useState("");
  const [manifestMaterialDigest, setManifestMaterialDigest] = useState("");
  const [manifestArtifacts, setManifestArtifacts] = useState("[]");
  const [manifestCapabilities, setManifestCapabilities] = useState("[]");

  const [connectionId, setConnectionId] = useState("");
  const [credentialRevision, setCredentialRevision] = useState("1");
  const [policyRevision, setPolicyRevision] = useState("1");
  const [policyModels, setPolicyModels] = useState("");
  const [policyDestinations, setPolicyDestinations] = useState("");
  const [policyProvider, setPolicyProvider] = useState("openrouter-fixed");
  const [policyDigest, setPolicyDigest] = useState("");
  const [policyScopes, setPolicyScopes] = useState("[]");
  const [textGenerate, setTextGenerate] = useState(true);
  const [smtpSend, setSmtpSend] = useState(true);

  const [agent, setAgent] = useState("");
  const [platform, setPlatform] = useState("");
  const [accountName, setAccountName] = useState("");
  const [scopes, setScopes] = useState("");

  const [revokeId, setRevokeId] = useState("");
  const [revokeRevision, setRevokeRevision] = useState("");

  // Native sources expose enroll/rotate only. There is no reviewed
  // authentication-only verification adapter: the broker refuses
  // `unsupported_verification`, and a stored key is not a verified sign-in.
  const [nativeVerb, setNativeVerb] = useState<"enroll" | "rotate">("enroll");
  const [nativeShape, setNativeShape] = useState<"native-model-api-key" | "native-model-devin-credentials">(
    "native-model-api-key",
  );
  const [nativeTarget, setNativeTarget] = useState("");
  const [nativeSourceId, setNativeSourceId] = useState("");
  const [nativeRequest, setNativeRequest] = useState<CredentialRequest | null>(null);

  const limitValues = useMemo(() => {
    const build = (row: string[]) => ({
      requests_per_60s: counter(row[0] ?? ""),
      unresolved: counter(row[1] ?? ""),
      daily_micros: dollarsToMicros(row[2] ?? ""),
      lifetime_micros: dollarsToMicros(row[3] ?? ""),
      per_call_micros: dollarsToMicros(row[4] ?? ""),
    });
    return { principal: build(principal), account: build(account), workspace: build(workspace) };
  }, [principal, account, workspace]);

  const run = async (action: string, params: Record<string, unknown>) => {
    setBusy(true);
    setError(null);
    setNotice(null);
    let issued: Attempt | null = null;
    try {
      const session = await api.protectedManagementSession({ action, params });
      // Record the attempt id BEFORE the one-use action. A lost HTTP
      // response or a failed checkpoint acknowledgement must still name
      // THIS attempt, so reconciliation cannot check a previous one.
      issued = { id: session.attempt_id, state: "issued", code: null, action };
      setAttempt(issued);
      const result = await api.protectedManagementAction({
        attempt_id: session.attempt_id,
        nonce: session.nonce,
        action,
        params,
      });
      const state = typeof result.state === "string" ? result.state : "applied";
      const code = typeof result.code === "string" ? result.code : null;
      setAttempt({ id: session.attempt_id, state, code, action });
      setNotice(`${action}: ${state}`);
    } catch (failure) {
      const code = failure instanceof ApiError ? failure.code ?? null : null;
      if (issued !== null) {
        // Ambiguous outcome (lost response, checkpoint failure): keep the
        // issued id with an unknown state. "Check status" reads that exact
        // attempt and never resubmits the mutation.
        setAttempt({ id: issued.id, state: "unknown", code, action });
        setNotice("Outcome unknown — reconcile this attempt's status below. Do not retry.");
      } else {
        setError(errorCopy(failure));
      }
    } finally {
      setBusy(false);
    }
  };

  const checkStatus = async () => {
    if (attempt === null) return;
    setBusy(true);
    setError(null);
    try {
      const status = await api.protectedManagementStatus(attempt.id);
      setAttempt({
        id: status.attempt_id,
        state: status.state,
        code: status.code,
        action: status.action,
      });
    } catch (failure) {
      setError(errorCopy(failure));
    } finally {
      setBusy(false);
    }
  };

  const simple = (action: string, label: string) => (
    <Button
      variant="ghost"
      size="sm"
      disabled={!canWrite || busy}
      onClick={() => void run(action, {})}
    >
      {label}
    </Button>
  );

  const limitsValid = Object.values(limitValues).every((row) =>
    Object.values(row).every((value) => value !== null),
  );

  const submitConfig = () => {
    if (!limitsValid) {
      setError("Enter finite whole counters and dollar amounts with at most 6 decimals.");
      return;
    }
    void run("config", { enabled, limits: limitValues });
  };

  const submitManifest = () => {
    let artifacts: unknown;
    let capabilities: unknown;
    try {
      artifacts = JSON.parse(manifestArtifacts);
      capabilities = JSON.parse(manifestCapabilities);
    } catch {
      setError("The manifest artifact/capability lists must be valid JSON. Nothing was changed.");
      return;
    }
    void run("manifest", {
      manifest: {
        v: 1,
        manifest_id: manifestId.trim(),
        revision: manifestRevision.trim(),
        artifacts,
        capabilities,
        policy_digest: manifestPolicyDigest.trim(),
        material_digest: manifestMaterialDigest.trim(),
      },
    });
  };

  const submitPolicy = () => {
    let dataScopes: unknown;
    try {
      dataScopes = JSON.parse(policyScopes);
    } catch {
      setError("The data-scope list must be valid JSON. Nothing was changed.");
      return;
    }
    const operations = [
      ...(textGenerate ? ["text.generate"] : []),
      ...(smtpSend ? ["smtp.send"] : []),
    ];
    void run("policy", {
      connection_id: connectionId.trim(),
      credential_revision: Number(credentialRevision),
      policy_digest: policyDigest.trim(),
      policy: {
        policy_revision: policyRevision.trim(),
        active: true,
        allowed_operations: operations,
        allowed_human_operations: ["smtp.verify"],
        allowed_destinations: list(policyDestinations),
        allowed_models: list(policyModels),
        provider_profile: policyProvider.trim(),
        data_scopes: dataScopes,
        approved_document_digest: policyDigest.trim(),
      },
    });
  };

  const submitGrant = (grant: boolean) => {
    void run(grant ? "grant" : "ungrant", {
      agent: agent.trim(),
      platform: platform.trim(),
      account: accountName.trim(),
      scopes: list(scopes),
    });
  };

  const submitRevoke = () => {
    const revision = revokeRevision.trim();
    void run("revoke_credential", {
      connection_id: revokeId.trim(),
      ...(revision ? { expected_credential_revision: Number(revision) } : {}),
    });
  };

  const createNativeRequest = async () => {
    setBusy(true);
    setError(null);
    setNativeRequest(null);
    try {
      const reply = await api.credentialRequestCreate({
        provider: "devin",
        verb: nativeVerb,
        shape: nativeShape,
        ...(nativeVerb !== "enroll" && nativeTarget.trim()
          ? { target_id: nativeTarget.trim() }
          : {}),
      });
      setNativeRequest(reply.request);
    } catch (failure) {
      setError(errorCopy(failure));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="card px-4 py-3.5 min-w-0 space-y-4" aria-label={t("Protected custody and approvals")}>
      <div>
        <h2 className="slabel">{t("Protected custody and approvals")}</h2>
        <p className="text-micro text-ink-500 mt-1">
          {t("Each action uses one one-use management attempt bound to this signed-in operator session. No credential is typed here.")}
        </p>
      </div>

      {!canWrite && (
        <p className="text-label text-ink-500">
          {t("Operator write access is required for these controls.")}
        </p>
      )}

      {error !== null && (
        <p className="text-label text-amber-400 break-words" role="alert">{error}</p>
      )}
      {notice !== null && (
        <p className="text-label text-ink-300 break-words" role="status">{notice}</p>
      )}
      {attempt !== null && (
        <div className="flex flex-wrap items-center gap-2 text-micro text-ink-400">
          <span>{t("Attempt")}: <code>{attempt.id}</code></span>
          <span>{t("state")}: {attempt.state ?? "unknown"}</span>
          {attempt.code !== null && <span>{t("code")}: {attempt.code}</span>}
          <Button variant="ghost" size="sm" disabled={busy} onClick={() => void checkStatus()}>
            {t("Check status")}
          </Button>
        </div>
      )}

      <div className="flex flex-wrap gap-2 items-center">
        <span className="text-label text-ink-400">{t("Key session")}</span>
        {simple("custody_initialize", t("Initialize custody"))}
        {simple("custody_unlock", t("Unlock"))}
        {simple("custody_lock", t("Lock"))}
        <span className="text-micro text-ink-500">
          {t("Initialize provisions the key header from the operator key file; it does not unlock or enable anything.")}
        </span>
      </div>

      <div className="space-y-2">
        <h3 className="text-label text-ink-300">{t("Owner limits and enablement")}</h3>
        <label className="flex items-center gap-2 text-label text-ink-300">
          <input
            type="checkbox"
            checked={enabled}
            onChange={(event) => setEnabled(event.target.checked)}
          />
          {t("Enable protected Connections")}
        </label>
        <LimitRow label={t("principal")} values={principal} onChange={(i, v) => setPrincipal((row) => row.map((x, n) => (n === i ? v : x)))} />
        <LimitRow label={t("account")} values={account} onChange={(i, v) => setAccount((row) => row.map((x, n) => (n === i ? v : x)))} />
        <LimitRow label={t("workspace")} values={workspace} onChange={(i, v) => setWorkspace((row) => row.map((x, n) => (n === i ? v : x)))} />
        <Button variant="ghost" size="sm" disabled={!canWrite || busy} onClick={submitConfig}>
          {t("Apply limits")}
        </Button>
      </div>

      <details className="space-y-2">
        <summary className="text-label text-ink-300 cursor-pointer">{t("Approve manifest")}</summary>
        <div className="grid gap-2 sm:grid-cols-2">
          <input className="field" placeholder={t("manifest id")} value={manifestId} onChange={(e) => setManifestId(e.target.value)} aria-label={t("manifest id")} />
          <input className="field" placeholder={t("revision")} value={manifestRevision} onChange={(e) => setManifestRevision(e.target.value)} aria-label={t("manifest revision")} />
          <input className="field" placeholder={t("policy digest sha256:…")} value={manifestPolicyDigest} onChange={(e) => setManifestPolicyDigest(e.target.value)} aria-label={t("manifest policy digest")} />
          <input className="field" placeholder={t("material digest sha256:…")} value={manifestMaterialDigest} onChange={(e) => setManifestMaterialDigest(e.target.value)} aria-label={t("manifest material digest")} />
        </div>
        <label className="block text-micro text-ink-500">
          {t("artifacts (exact typed JSON)")}
          <textarea className="field w-full h-24 font-mono" value={manifestArtifacts} onChange={(e) => setManifestArtifacts(e.target.value)} />
        </label>
        <label className="block text-micro text-ink-500">
          {t("capabilities (exact typed JSON)")}
          <textarea className="field w-full h-24 font-mono" value={manifestCapabilities} onChange={(e) => setManifestCapabilities(e.target.value)} />
        </label>
        <Button variant="ghost" size="sm" disabled={!canWrite || busy} onClick={submitManifest}>
          {t("Approve manifest")}
        </Button>
      </details>

      <details className="space-y-2">
        <summary className="text-label text-ink-300 cursor-pointer">{t("Connection policy")}</summary>
        <div className="grid gap-2 sm:grid-cols-2">
          <input className="field" placeholder={t("connection id")} value={connectionId} onChange={(e) => setConnectionId(e.target.value)} aria-label={t("connection id")} />
          <input className="field" placeholder={t("credential revision")} value={credentialRevision} onChange={(e) => setCredentialRevision(e.target.value)} aria-label={t("credential revision")} />
          <input className="field" placeholder={t("policy revision")} value={policyRevision} onChange={(e) => setPolicyRevision(e.target.value)} aria-label={t("policy revision")} />
          <input className="field" placeholder={t("provider profile")} value={policyProvider} onChange={(e) => setPolicyProvider(e.target.value)} aria-label={t("provider profile")} />
          <input className="field" placeholder={t("allowed models, comma separated")} value={policyModels} onChange={(e) => setPolicyModels(e.target.value)} aria-label={t("allowed models")} />
          <input className="field" placeholder={t("allowed destinations, comma separated")} value={policyDestinations} onChange={(e) => setPolicyDestinations(e.target.value)} aria-label={t("allowed destinations")} />
          <input className="field" placeholder={t("approved document digest sha256:…")} value={policyDigest} onChange={(e) => setPolicyDigest(e.target.value)} aria-label={t("approved document digest")} />
        </div>
        <div className="flex gap-3 text-label text-ink-300">
          <label className="flex items-center gap-2">
            <input type="checkbox" checked={textGenerate} onChange={(e) => setTextGenerate(e.target.checked)} />
            {t("text.generate")}
          </label>
          <label className="flex items-center gap-2">
            <input type="checkbox" checked={smtpSend} onChange={(e) => setSmtpSend(e.target.checked)} />
            {t("smtp.send")}
          </label>
        </div>
        <label className="block text-micro text-ink-500">
          {t("data scopes (exact typed JSON)")}
          <textarea className="field w-full h-24 font-mono" value={policyScopes} onChange={(e) => setPolicyScopes(e.target.value)} />
        </label>
        <Button variant="ghost" size="sm" disabled={!canWrite || busy} onClick={submitPolicy}>
          {t("Apply policy")}
        </Button>
      </details>

      <details className="space-y-2">
        <summary className="text-label text-ink-300 cursor-pointer">{t("Agent UID grant")}</summary>
        <div className="grid gap-2 sm:grid-cols-2">
          <input className="field" placeholder={t("agent alias")} value={agent} onChange={(e) => setAgent(e.target.value)} aria-label={t("agent alias")} />
          <input className="field" placeholder={t("platform")} value={platform} onChange={(e) => setPlatform(e.target.value)} aria-label={t("platform")} />
          <input className="field" placeholder={t("account")} value={accountName} onChange={(e) => setAccountName(e.target.value)} aria-label={t("account")} />
          <input className="field" placeholder={t("scopes, comma separated")} value={scopes} onChange={(e) => setScopes(e.target.value)} aria-label={t("scopes")} />
        </div>
        <div className="flex flex-wrap gap-2">
          <Button variant="ghost" size="sm" disabled={!canWrite || busy} onClick={() => submitGrant(true)}>
            {t("Grant")}
          </Button>
          <Button variant="ghost" size="sm" disabled={!canWrite || busy} onClick={() => submitGrant(false)}>
            {t("Ungrant")}
          </Button>
        </div>
        <p className="text-micro text-ink-500">
          {t("Grants bind the agent's stable UID at grant time; an alias change requires an explicit new grant.")}
        </p>
      </details>

      <details className="space-y-2">
        <summary className="text-label text-ink-300 cursor-pointer">{t("Revoke credential")}</summary>
        <div className="grid gap-2 sm:grid-cols-2">
          <input className="field" placeholder={t("connection id")} value={revokeId} onChange={(e) => setRevokeId(e.target.value)} aria-label={t("revoke connection id")} />
          <input className="field" placeholder={t("expected credential revision (optional)")} value={revokeRevision} onChange={(e) => setRevokeRevision(e.target.value)} aria-label={t("revoke revision")} />
        </div>
        <Button variant="ghost" size="sm" disabled={!canWrite || busy} onClick={submitRevoke}>
          {t("Revoke credential")}
        </Button>
      </details>

      <div className="space-y-2 border-t border-ink-800 pt-3">
        <h3 className="text-label text-ink-300">{t("Native model source")}</h3>
        <p className="text-micro text-ink-500">
          {t("The native model broker keeps its own encrypted source. Choose a verb and shape, create a metadata-only request, then type the key or credential file on the isolated entry page.")}
        </p>
        <div className="flex flex-wrap gap-2 items-end">
          <label className="text-micro text-ink-500 flex flex-col gap-1">
            {t("verb")}
            <select className="field" value={nativeVerb} onChange={(e) => setNativeVerb(e.target.value as typeof nativeVerb)}>
              <option value="enroll">{t("enroll")}</option>
              <option value="rotate">{t("rotate")}</option>
            </select>
          </label>
          <label className="text-micro text-ink-500 flex flex-col gap-1">
            {t("shape")}
            <select className="field" value={nativeShape} onChange={(e) => setNativeShape(e.target.value as typeof nativeShape)}>
              <option value="native-model-api-key">{t("API key")}</option>
              <option value="native-model-devin-credentials">{t("Devin credential file")}</option>
            </select>
          </label>
          {nativeVerb !== "enroll" && (
            <input className="field" placeholder={t("source id")} value={nativeTarget} onChange={(e) => setNativeTarget(e.target.value)} aria-label={t("native model source id")} />
          )}
          <Button variant="ghost" size="sm" disabled={!canWrite || busy} onClick={() => void createNativeRequest()}>
            {t("Create request")}
          </Button>
        </div>
        {nativeRequest !== null &&
          (entryHref(nativeRequest) !== null ? (
            <p className="text-label">
              <a href={entryHref(nativeRequest) ?? undefined} className="lnk">
                {t("Open the trusted entry page")}
              </a>
            </p>
          ) : (
            <p className="text-micro text-warn" role="status">
              {t("Trusted credential entry is unsupported on this deployment — no form is served on this board.")}
            </p>
          ))}
        <div className="grid gap-2 sm:grid-cols-2">
          <input
            className="field"
            placeholder={t("native source id")}
            value={nativeSourceId}
            onChange={(e) => setNativeSourceId(e.target.value)}
            aria-label={t("native model source id to lock or revoke")}
          />
          <div className="flex flex-wrap gap-2">
            <Button
              variant="ghost"
              size="sm"
              disabled={!canWrite || busy || nativeSourceId.trim() === ""}
              onClick={() => void run("native_source_lock", { source_id: nativeSourceId.trim() })}
            >
              {t("Lock source")}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              disabled={!canWrite || busy || nativeSourceId.trim() === ""}
              onClick={() => void run("native_source_revoke", { source_id: nativeSourceId.trim() })}
            >
              {t("Revoke source")}
            </Button>
          </div>
        </div>
        <p className="text-micro text-ink-500">
          {t("Locking/revoking fences the broker source at its actual owner epoch and checkpoint; it prevents new model turns. This is metadata-only and never reads or verifies the provider credential. Only an authorized model turn authenticates a source; a stored key is not a verified sign-in.")}
        </p>
      </div>
    </section>
  );
}
