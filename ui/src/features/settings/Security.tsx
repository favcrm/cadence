import { useCallback, useEffect, useId, useRef, useState, type FormEvent } from "react";
import { ApiError } from "../../lib/apiError";
import { useTranslate } from "../../lib/locale";
import { onPrivateSessionReset, privateFetch, withPrivateOperation } from "../../lib/privateSession";
import { sessionHeaders } from "../../lib/sessionKey";
import Button from "../../ui/Button";
import Switch from "../../ui/Switch";
import Toast, { type ToastMsg } from "../../ui/Toast";
import type { Viewer } from "../projects/work";
import WorkspaceSecurityPolicy from "./WorkspaceSecurityPolicy";
import { encodeQrMatrix, qrSvgPath } from "./qrCode";
import "./security.css";

type Translate = (english: string) => string;

type RequiredWorkspace = { id: string; name: string };

export type AccountSecurityState = {
  email: string;
  hasPassword: boolean;
  twoFactorEnabled: boolean;
  backupCodesRemaining: number;
  requiredByWorkspaces: RequiredWorkspace[];
};

type TwoFactorSetup = { totpURI: string; backupCodes: string[] };

type TwoFactorPanel = "none" | "enable-password" | "verify" | "regen" | "disable";

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function errorCodeOf(body: unknown): string | undefined {
  if (!isRecord(body)) return undefined;
  if (typeof body.code === "string") return body.code;
  const nested = body.error;
  return isRecord(nested) && typeof nested.code === "string" ? nested.code : undefined;
}

function errorMessageOf(body: unknown, fallback: string): string {
  if (isRecord(body)) {
    if (typeof body.message === "string" && body.message.trim()) return body.message;
    const nested = body.error;
    if (isRecord(nested) && typeof nested.message === "string" && nested.message.trim()) {
      return nested.message;
    }
  }
  return fallback;
}

function buildApiError(body: unknown, response: Response): ApiError {
  return new ApiError(errorMessageOf(body, `${response.status} ${response.statusText}`), response.status, {
    code: errorCodeOf(body),
  });
}

function extractSecret(totpURI: string): string {
  try {
    return new URL(totpURI).searchParams.get("secret") ?? "";
  } catch {
    return "";
  }
}

function downloadBackupCodes(codes: string[], heading: string): void {
  if (typeof document === "undefined" || typeof URL === "undefined" || typeof URL.createObjectURL !== "function") {
    return;
  }
  const content = `${heading}\n\n${codes.join("\n")}\n`;
  const blob = new Blob([content], { type: "text/plain;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = "cadence-backup-codes.txt";
  link.click();
  URL.revokeObjectURL(url);
}

function isRequiredWorkspace(entry: unknown): entry is RequiredWorkspace {
  return isRecord(entry) && typeof entry.id === "string" && typeof entry.name === "string";
}

function replaceToken(template: string, token: string, value: string): string {
  return template.replace(token, () => value);
}

function parseSecurityState(payload: unknown): AccountSecurityState {
  if (
    !isRecord(payload) ||
    typeof payload.email !== "string" ||
    typeof payload.hasPassword !== "boolean" ||
    typeof payload.twoFactorEnabled !== "boolean" ||
    typeof payload.backupCodesRemaining !== "number"
  ) {
    throw new Error("unexpected security state");
  }
  const rawWorkspaces = Array.isArray(payload.requiredByWorkspaces) ? payload.requiredByWorkspaces : [];
  const requiredByWorkspaces: RequiredWorkspace[] = rawWorkspaces.filter(isRequiredWorkspace);
  return {
    email: payload.email,
    hasPassword: payload.hasPassword,
    twoFactorEnabled: payload.twoFactorEnabled,
    backupCodesRemaining: payload.backupCodesRemaining,
    requiredByWorkspaces,
  };
}

function translateSecurityError(error: unknown, t: Translate, fallback: string): string {
  if (!(error instanceof ApiError)) return t(fallback);
  switch (error.code) {
    case "invalid_password":
      return t("Current password is incorrect.");
    case "password_already_set":
      return t("A password is already set for this account.");
    case "password_not_set":
      return t("Set a password before attempting to change it.");
    case "invalid_totp_code":
      return t("That verification code didn’t match. Try the latest 6-digit code.");
    case "two_factor_already_enabled":
      return t("Two-factor authentication is already enabled.");
    case "two_factor_not_enabled":
      return t("Two-factor authentication is not enabled.");
    case "two_factor_not_started":
      return t("Two-factor setup expired. Start again.");
    case "two_factor_required_by_org":
      return t("Two-factor authentication is required by workspace policy and cannot be disabled.");
    case "rate_limited":
      return t("Too many attempts. Please wait a moment and try again.");
    case "account_temporarily_locked":
      return t("Too many failed attempts. Please wait before trying again.");
    case "operator_unsupported":
      return t("Account security settings are personal to a signed-in user.");
    case "email_delivery_failed":
      return t("Could not send the password reset email right now. Try again later.");
    case "membership_changed":
      return t("Your access changed. Reload the page and try again.");
    case "validation_failed":
      return t("Check your input and try again.");
    default:
      return t(fallback);
  }
}

async function readSecurity(signal?: AbortSignal): Promise<AccountSecurityState> {
  return withPrivateOperation(async (operation) => {
    const response = await privateFetch("/api/account/security", { headers: sessionHeaders() }, operation);
    const parsed: unknown = await response.json().catch(() => null);
    operation.assertCurrent();
    if (!response.ok) throw buildApiError(parsed, response);
    return parseSecurityState(parsed);
  }, signal);
}

async function postSecurity(path: string, body: object): Promise<unknown> {
  return withPrivateOperation(async (operation) => {
    const response = await privateFetch(
      path,
      {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "X-Cadence-Board": "1",
          ...sessionHeaders(),
        },
        body: JSON.stringify(body),
      },
      operation,
    );
    const parsed: unknown = await response.json().catch(() => null);
    operation.assertCurrent();
    if (!response.ok) throw buildApiError(parsed, response);
    if (parsed === null) {
      throw new ApiError("The board answered without a readable receipt.", response.status);
    }
    return parsed;
  });
}

export default function Security({
  onSessionInvalidated,
  viewer,
  hosted,
  policyScope,
}: {
  onSessionInvalidated?: () => void;
  viewer?: Viewer;
  hosted?: boolean;
  policyScope?: string;
}) {
  const id = useId();
  const t = useTranslate();
  const tRef = useRef(t);
  tRef.current = t;
  const [state, setState] = useState<AccountSecurityState | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const loadGen = useRef(0);

  // Password form state
  const [pwOpen, setPwOpen] = useState(false);
  const [currentPassword, setCurrentPassword] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [showPw, setShowPw] = useState(false);
  const [revokeOtherSessions, setRevokeOtherSessions] = useState(true);
  const [pwBusy, setPwBusy] = useState(false);
  const [pwError, setPwError] = useState<string | null>(null);
  const [resetBusy, setResetBusy] = useState(false);
  const [resetNotice, setResetNotice] = useState<string | null>(null);
  const [resetError, setResetError] = useState<string | null>(null);
  const [reauthNotice, setReauthNotice] = useState(false);

  // Two-factor state
  const [tfaPanel, setTfaPanel] = useState<TwoFactorPanel>("none");
  const [tfaPassword, setTfaPassword] = useState("");
  const [tfaBusy, setTfaBusy] = useState(false);
  const [tfaError, setTfaError] = useState<string | null>(null);
  const [setup, setSetup] = useState<TwoFactorSetup | null>(null);
  const [verifyCode, setVerifyCode] = useState("");
  const [revealedCodes, setRevealedCodes] = useState<string[] | null>(null);
  const [copiedTarget, setCopiedTarget] = useState<"secret" | "codes" | null>(null);

  const [toast, setToast] = useState<ToastMsg | null>(null);
  const toastTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const copyTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const showToast = useCallback((kind: ToastMsg["kind"], text: string) => {
    if (toastTimer.current) clearTimeout(toastTimer.current);
    setToast({ kind, text });
    toastTimer.current = setTimeout(() => setToast(null), 3500);
  }, []);

  useEffect(() => {
    return () => {
      if (toastTimer.current) clearTimeout(toastTimer.current);
      if (copyTimer.current) clearTimeout(copyTimer.current);
    };
  }, []);

  const clearSecrets = useCallback(() => {
    setCurrentPassword("");
    setNewPassword("");
    setConfirmPassword("");
    setShowPw(false);
    setPwOpen(false);
    setPwError(null);
    setResetNotice(null);
    setResetError(null);
    setTfaPanel("none");
    setTfaPassword("");
    setTfaError(null);
    setSetup(null);
    setVerifyCode("");
    setRevealedCodes(null);
    setCopiedTarget(null);
  }, []);

  const load = useCallback(async (signal?: AbortSignal) => {
    const gen = ++loadGen.current;
    setLoading(true);
    setLoadError(null);
    try {
      const next = await readSecurity(signal);
      if (gen !== loadGen.current || signal?.aborted) return;
      setState(next);
    } catch (failure) {
      if (gen !== loadGen.current || signal?.aborted) return;
      setLoadError(translateSecurityError(failure, tRef.current, "Could not load account security settings."));
    } finally {
      if (gen === loadGen.current && !signal?.aborted) {
        setLoading(false);
      }
    }
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    void load(controller.signal);
    return () => controller.abort();
  }, [load]);

  useEffect(() => {
    return onPrivateSessionReset(() => {
      clearSecrets();
      setState(null);
      setLoadError(null);
      setToast(null);
      setReauthNotice(false);
    });
  }, [clearSecrets]);

  const copyText = async (value: string, target: "secret" | "codes") => {
    if (typeof navigator === "undefined" || !navigator.clipboard?.writeText) {
      showToast("warn", t("Copy unavailable. Select and copy manually."));
      return;
    }
    try {
      await navigator.clipboard.writeText(value);
      setCopiedTarget(target);
      if (copyTimer.current) clearTimeout(copyTimer.current);
      copyTimer.current = setTimeout(() => setCopiedTarget((cur) => (cur === target ? null : cur)), 2000);
    } catch {
      showToast("warn", t("Copy unavailable. Select and copy manually."));
    }
  };

  const resetPasswordForm = () => {
    setPwOpen(false);
    setCurrentPassword("");
    setNewPassword("");
    setConfirmPassword("");
    setShowPw(false);
    setPwError(null);
    setResetNotice(null);
    setResetError(null);
  };

  const submitPassword = async (event: FormEvent) => {
    event.preventDefault();
    if (!state || pwBusy) return;
    setPwError(null);
    setResetNotice(null);
    setResetError(null);

    if (state.hasPassword && !currentPassword) {
      setPwError(t("Current password is required."));
      return;
    }
    if (newPassword.length < 8 || newPassword.length > 128) {
      setPwError(t("Password must be between 8 and 128 characters."));
      return;
    }
    if (newPassword !== confirmPassword) {
      setPwError(t("Passwords do not match."));
      return;
    }

    setPwBusy(true);
    try {
      if (state.hasPassword) {
        const result = await postSecurity("/api/account/security/password/change", {
          currentPassword,
          newPassword,
          revokeOtherSessions,
        });
        const revoked =
          isRecord(result) && typeof result.revokedOtherBoardSessions === "number"
            ? result.revokedOtherBoardSessions
            : 0;
        const reauthRequired = isRecord(result) && result.reauthRequired === true;
        setState((prev) => (prev ? { ...prev, hasPassword: true } : prev));
        resetPasswordForm();
        if (reauthRequired) {
          setReauthNotice(true);
          onSessionInvalidated?.();
        } else if (revoked === 1) {
          showToast("ok", t("Password changed. Signed out 1 other session."));
        } else if (revoked > 1) {
          showToast(
            "ok",
            replaceToken(t("Password changed. Signed out {count} other sessions."), "{count}", String(revoked)),
          );
        } else {
          showToast("ok", t("Password changed."));
        }
      } else {
        await postSecurity("/api/account/security/password/set", { newPassword });
        setState((prev) => (prev ? { ...prev, hasPassword: true } : prev));
        resetPasswordForm();
        showToast("ok", t("Password set."));
      }
    } catch (failure) {
      setPwError(translateSecurityError(failure, t, "Could not update password. Try again."));
    } finally {
      setPwBusy(false);
    }
  };

  const triggerResetEmail = async () => {
    if (resetBusy || pwBusy) return;
    setResetBusy(true);
    setResetNotice(null);
    setResetError(null);
    try {
      const res = await postSecurity("/api/account/security/password/reset-email", {});
      const targetEmail = isRecord(res) && typeof res.email === "string" ? res.email : state?.email ?? "";
      setResetNotice(replaceToken(t("Reset link sent to {email}."), "{email}", targetEmail));
    } catch (failure) {
      setResetError(translateSecurityError(failure, t, "Could not send password reset email."));
    } finally {
      setResetBusy(false);
    }
  };

  const startEnableTwoFactor = async (passwordArg?: string) => {
    if (tfaBusy) return;
    setTfaBusy(true);
    setTfaError(null);
    try {
      const payload = state?.hasPassword ? { password: passwordArg ?? tfaPassword } : {};
      const res = await postSecurity("/api/account/security/two-factor/enable", payload);
      if (
        !isRecord(res) ||
        typeof res.totpURI !== "string" ||
        !Array.isArray(res.backupCodes) ||
        !res.backupCodes.every((c): c is string => typeof c === "string")
      ) {
        throw new Error("unexpected 2fa setup payload");
      }
      setTfaPassword("");
      setSetup({ totpURI: res.totpURI, backupCodes: res.backupCodes });
      setVerifyCode("");
      setTfaPanel("verify");
    } catch (failure) {
      setTfaError(translateSecurityError(failure, t, "Could not start two-factor setup."));
    } finally {
      setTfaBusy(false);
    }
  };

  const submitEnableClick = () => {
    if (!state || tfaBusy) return;
    setTfaError(null);
    setRevealedCodes(null);
    if (state.hasPassword) {
      setTfaPassword("");
      setTfaPanel("enable-password");
      return;
    }
    void startEnableTwoFactor();
  };

  const submitVerifyTwoFactor = async (event: FormEvent) => {
    event.preventDefault();
    if (!setup || tfaBusy) return;
    const cleaned = verifyCode.replace(/\s+/g, "");
    if (!/^\d{6}$/.test(cleaned)) {
      setTfaError(t("Enter a 6-digit authentication code."));
      return;
    }
    setTfaBusy(true);
    setTfaError(null);
    try {
      await postSecurity("/api/account/security/two-factor/verify", { code: cleaned });
      const codes = setup.backupCodes;
      setState((prev) =>
        prev
          ? { ...prev, twoFactorEnabled: true, backupCodesRemaining: codes.length }
          : prev,
      );
      setSetup(null);
      setVerifyCode("");
      setTfaPanel("none");
      setRevealedCodes(codes);
      showToast("ok", t("Two-factor authentication enabled."));
    } catch (failure) {
      setTfaError(translateSecurityError(failure, t, "Could not verify the authentication code."));
    } finally {
      setTfaBusy(false);
    }
  };

  const submitRegenerateBackupCodes = async (event: FormEvent) => {
    event.preventDefault();
    if (!state || tfaBusy) return;
    if (state.hasPassword && !tfaPassword) {
      setTfaError(t("Current password is required."));
      return;
    }
    setTfaBusy(true);
    setTfaError(null);
    try {
      const payload = state.hasPassword ? { password: tfaPassword } : {};
      const res = await postSecurity("/api/account/security/two-factor/backup-codes", payload);
      if (!isRecord(res) || !Array.isArray(res.backupCodes)) {
        throw new Error("unexpected backup codes payload");
      }
      const nextCodes = res.backupCodes.filter((c): c is string => typeof c === "string");
      if (nextCodes.length !== res.backupCodes.length) {
        throw new Error("unexpected backup codes payload");
      }
      setTfaPassword("");
      setTfaPanel("none");
      setRevealedCodes(nextCodes);
      setState((prev) =>
        prev ? { ...prev, backupCodesRemaining: nextCodes.length } : prev,
      );
      showToast("ok", t("New backup codes generated."));
    } catch (failure) {
      setTfaError(translateSecurityError(failure, t, "Could not regenerate backup codes."));
    } finally {
      setTfaBusy(false);
    }
  };

  const submitDisableTwoFactor = async (event: FormEvent) => {
    event.preventDefault();
    if (!state || tfaBusy) return;
    if (state.hasPassword && !tfaPassword) {
      setTfaError(t("Current password is required."));
      return;
    }
    setTfaBusy(true);
    setTfaError(null);
    try {
      const payload = state.hasPassword ? { password: tfaPassword } : {};
      await postSecurity("/api/account/security/two-factor/disable", payload);
      setTfaPassword("");
      setTfaPanel("none");
      setRevealedCodes(null);
      setState((prev) =>
        prev ? { ...prev, twoFactorEnabled: false, backupCodesRemaining: 0 } : prev,
      );
      showToast("ok", t("Two-factor authentication disabled."));
    } catch (failure) {
      setTfaError(translateSecurityError(failure, t, "Could not disable two-factor authentication."));
    } finally {
      setTfaBusy(false);
    }
  };

  const qrMatrix = setup ? encodeQrMatrix(setup.totpURI) : null;
  const manualSecret = setup ? extractSecret(setup.totpURI) : "";
  const orgLocked = (state?.requiredByWorkspaces.length ?? 0) > 0;
  const requiredNames = state?.requiredByWorkspaces.map((w) => w.name || w.id).join(", ") ?? "";

  return (
    <main className="px-4 lg:px-8 py-5 min-w-0 max-w-3xl">
      <header>
        <h1 className="text-section font-semibold text-ink-100">{t("Security")}</h1>
        <p className="mt-1 text-secondary text-ink-400">
          {t("Manage your password and two-factor authentication.")}
        </p>
      </header>

      {reauthNotice && (
        <div role="status" className="security-banner security-banner-warn">
          {t("Your password changed. Because a workspace requires two-factor authentication, sign in again to continue.")}
        </div>
      )}

      {loading && !state && (
        <p role="status" className="mt-7 text-secondary text-ink-400">
          {t("Loading security settings…")}
        </p>
      )}

      {!loading && loadError && !state && (
        <div className="mt-7 flex flex-wrap items-center justify-between gap-3 rounded-lg border border-ink-700 p-5">
          <p role="alert" className="security-error">{loadError}</p>
          <Button onClick={() => void load()}>{t("Retry")}</Button>
        </div>
      )}

      {state && (
        <div className="mt-7">
          <section className="security-section" aria-labelledby={`${id}-password`}>
            <div className="security-row-head">
              <div className="security-row-meta">
                <h2 id={`${id}-password`} className="text-cardtitle font-medium text-ink-100">
                  {t("Password")}
                </h2>
                <div className="security-status-line">
                  <span className="text-label text-ink-400">{state.email}</span>
                  <span className={`security-chip ${state.hasPassword ? "security-chip-ok" : ""}`}>
                    {t(state.hasPassword ? "Password set" : "No password set")}
                  </span>
                </div>
                <p className="mt-1.5 text-label leading-relaxed text-ink-400">
                  {t(
                    state.hasPassword
                      ? "Sign in with your password or an email verification code."
                      : "You currently sign in with email codes. Set a password if you prefer password sign-in.",
                  )}
                </p>
              </div>
              {!pwOpen && (
                <Button
                  onClick={() => {
                    setPwError(null);
                    setResetNotice(null);
                    setResetError(null);
                    setPwOpen(true);
                  }}
                >
                  {t(state.hasPassword ? "Change password" : "Set password")}
                </Button>
              )}
            </div>

            {pwOpen && (
              <form className="security-panel" onSubmit={submitPassword}>
                {state.hasPassword && (
                  <div className="security-field">
                    <div className="security-label-row">
                      <label htmlFor={`${id}-cur-pw`} className="text-label text-ink-300">
                        {t("Current password")}
                      </label>
                      <button
                        type="button"
                        className="security-link-btn"
                        disabled={resetBusy || pwBusy}
                        onClick={() => void triggerResetEmail()}
                      >
                        {t(resetBusy ? "Sending reset email…" : "Forgot password?")}
                      </button>
                    </div>
                    <div className="security-input-wrap">
                      <input
                        id={`${id}-cur-pw`}
                        type={showPw ? "text" : "password"}
                        autoComplete="current-password"
                        value={currentPassword}
                        onChange={(e) => setCurrentPassword(e.target.value)}
                        className="field w-full"
                        disabled={pwBusy}
                      />
                      <button
                        type="button"
                        className="security-toggle-pw"
                        onClick={() => setShowPw((v) => !v)}
                        aria-pressed={showPw}
                        aria-label={t(showPw ? "Hide password" : "Show password")}
                      >
                        {t(showPw ? "Hide" : "Show")}
                      </button>
                    </div>
                    {resetNotice && (
                      <p role="status" className="security-status-msg">
                        {resetNotice}
                      </p>
                    )}
                    {resetError && (
                      <p role="alert" className="security-error">
                        {resetError}
                      </p>
                    )}
                  </div>
                )}

                <div className="security-field">
                  <div className="security-label-row">
                    <label htmlFor={`${id}-new-pw`} className="text-label text-ink-300">
                      {t("New password")}
                    </label>
                    {!state.hasPassword && (
                      <button
                        type="button"
                        className="security-link-btn"
                        aria-pressed={showPw}
                        onClick={() => setShowPw((v) => !v)}
                      >
                        {t(showPw ? "Hide password" : "Show password")}
                      </button>
                    )}
                  </div>
                  <input
                    id={`${id}-new-pw`}
                    type={showPw ? "text" : "password"}
                    autoComplete="new-password"
                    value={newPassword}
                    onChange={(e) => setNewPassword(e.target.value)}
                    placeholder={t("At least 8 characters")}
                    className="field w-full"
                    disabled={pwBusy}
                  />
                </div>

                <div className="security-field">
                  <label htmlFor={`${id}-confirm-pw`} className="text-label text-ink-300">
                    {t("Confirm password")}
                  </label>
                  <input
                    id={`${id}-confirm-pw`}
                    type={showPw ? "text" : "password"}
                    autoComplete="new-password"
                    value={confirmPassword}
                    onChange={(e) => setConfirmPassword(e.target.value)}
                    className="field w-full"
                    disabled={pwBusy}
                  />
                </div>

                {state.hasPassword && (
                  <div className="security-switch-row">
                    <div>
                      <p className="text-secondary font-medium text-ink-200">
                        {t("Sign out other sessions")}
                      </p>
                      <p className="text-label text-ink-400">
                        {t("Sign out other browsers and CLI devices while keeping this session.")}
                      </p>
                    </div>
                    <Switch
                      id={`${id}-revoke-others`}
                      checked={revokeOtherSessions}
                      onToggle={() => setRevokeOtherSessions((v) => !v)}
                      label={t("Sign out other sessions")}
                      disabled={pwBusy}
                    />
                  </div>
                )}

                {pwError && (
                  <p role="alert" className="security-error">
                    {pwError}
                  </p>
                )}

                <div className="security-actions">
                  <Button type="submit" variant="primary" loading={pwBusy}>
                    {t(state.hasPassword ? "Change password" : "Set password")}
                  </Button>
                  <Button variant="ghost" disabled={pwBusy} onClick={resetPasswordForm}>
                    {t("Cancel")}
                  </Button>
                </div>
              </form>
            )}
          </section>

          <section className="security-section" aria-labelledby={`${id}-tfa`}>
            <div className="security-row-head">
              <div className="security-row-meta">
                <h2 id={`${id}-tfa`} className="text-cardtitle font-medium text-ink-100">
                  {t("Two-factor authentication")}
                </h2>
                <div className="security-status-line">
                  <span className={`security-chip ${state.twoFactorEnabled ? "security-chip-ok" : ""}`}>
                    {t(state.twoFactorEnabled ? "Enabled" : "Not enabled")}
                  </span>
                  {state.twoFactorEnabled && (
                    <span className="text-label text-ink-400">
                      {state.backupCodesRemaining === 1
                        ? t("1 backup code remaining")
                        : replaceToken(
                            t("{count} backup codes remaining"),
                            "{count}",
                            String(state.backupCodesRemaining),
                          )}
                    </span>
                  )}
                </div>
                <p className="mt-1.5 text-label leading-relaxed text-ink-400">
                  {t("Require a 6-digit code from your authenticator app when signing in.")}
                </p>
                {orgLocked && (
                  <p className="mt-1.5 text-label text-ink-300">
                    {replaceToken(t("Required by workspace policy ({names})."), "{names}", requiredNames)}
                  </p>
                )}
              </div>

              {!state.twoFactorEnabled && tfaPanel === "none" && (
                <Button variant="primary" loading={tfaBusy} onClick={submitEnableClick}>
                  {t("Enable two-factor authentication")}
                </Button>
              )}

              {state.twoFactorEnabled && tfaPanel === "none" && (
                <div className="security-actions">
                  <Button
                    onClick={() => {
                      setTfaError(null);
                      setRevealedCodes(null);
                      setTfaPassword("");
                      setTfaPanel("regen");
                    }}
                  >
                    {t("Regenerate backup codes")}
                  </Button>
                  <Button
                    variant="danger"
                    disabled={orgLocked}
                    title={
                      orgLocked
                        ? replaceToken(t("Required by workspace policy ({names})."), "{names}", requiredNames)
                        : undefined
                    }
                    onClick={() => {
                      if (orgLocked) return;
                      setTfaError(null);
                      setRevealedCodes(null);
                      setTfaPassword("");
                      setTfaPanel("disable");
                    }}
                  >
                    {t("Disable")}
                  </Button>
                </div>
              )}
            </div>

            {tfaPanel === "none" && tfaError && (
              <p role="alert" className="security-error mt-3">
                {tfaError}
              </p>
            )}

            {tfaPanel === "enable-password" && (
              <form
                className="security-panel"
                onSubmit={(e) => {
                  e.preventDefault();
                  if (!tfaPassword) {
                    setTfaError(t("Current password is required."));
                    return;
                  }
                  void startEnableTwoFactor(tfaPassword);
                }}
              >
                <div className="security-field">
                  <label htmlFor={`${id}-enable-pw`} className="text-label text-ink-300">
                    {t("Confirm your password to set up two-factor authentication")}
                  </label>
                  <input
                    id={`${id}-enable-pw`}
                    type="password"
                    autoComplete="current-password"
                    value={tfaPassword}
                    onChange={(e) => setTfaPassword(e.target.value)}
                    className="field w-full"
                    disabled={tfaBusy}
                  />
                </div>
                {tfaError && (
                  <p role="alert" className="security-error">
                    {tfaError}
                  </p>
                )}
                <div className="security-actions">
                  <Button type="submit" variant="primary" loading={tfaBusy}>
                    {t("Continue")}
                  </Button>
                  <Button
                    variant="ghost"
                    disabled={tfaBusy}
                    onClick={() => {
                      setTfaPassword("");
                      setTfaError(null);
                      setTfaPanel("none");
                    }}
                  >
                    {t("Cancel")}
                  </Button>
                </div>
              </form>
            )}

            {tfaPanel === "verify" && setup && (
              <form className="security-panel" onSubmit={submitVerifyTwoFactor}>
                <p className="text-secondary text-ink-200">
                  {t("Scan this QR code with your authenticator app, or enter the setup key manually.")}
                </p>
                <div className="security-qr-wrap">
                  {qrMatrix && (
                    <div className="security-qr-frame">
                      <svg
                        viewBox={`-4 -4 ${qrMatrix.size + 8} ${qrMatrix.size + 8}`}
                        width={168}
                        height={168}
                        role="img"
                        aria-label={t("Authenticator QR code")}
                        shapeRendering="crispEdges"
                      >
                        <rect
                          x={-4}
                          y={-4}
                          width={qrMatrix.size + 8}
                          height={qrMatrix.size + 8}
                          fill="#ffffff"
                        />
                        <path d={qrSvgPath(qrMatrix)} fill="#000000" />
                      </svg>
                    </div>
                  )}
                  <div className="security-secret-box">
                    <span className="text-label text-ink-400">{t("Manual setup key")}</span>
                    <code className="security-secret-code">{manualSecret}</code>
                    <div className="security-actions">
                      <Button
                        size="sm"
                        onClick={() => void copyText(manualSecret, "secret")}
                        disabled={!manualSecret}
                      >
                        {t(copiedTarget === "secret" ? "Copied" : "Copy key")}
                      </Button>
                    </div>
                  </div>
                </div>

                <div className="security-field">
                  <label htmlFor={`${id}-verify-code`} className="text-label text-ink-300">
                    {t("Authentication code")}
                  </label>
                  <input
                    id={`${id}-verify-code`}
                    type="text"
                    inputMode="numeric"
                    autoComplete="one-time-code"
                    maxLength={6}
                    value={verifyCode}
                    onChange={(e) => setVerifyCode(e.target.value.replace(/\D/g, "").slice(0, 6))}
                    placeholder="123456"
                    className="field num w-40"
                    disabled={tfaBusy}
                  />
                </div>

                {tfaError && (
                  <p role="alert" className="security-error">
                    {tfaError}
                  </p>
                )}

                <div className="security-actions">
                  <Button type="submit" variant="primary" loading={tfaBusy}>
                    {t("Verify and enable")}
                  </Button>
                  <Button
                    variant="ghost"
                    disabled={tfaBusy}
                    onClick={() => {
                      setSetup(null);
                      setVerifyCode("");
                      setTfaError(null);
                      setTfaPanel("none");
                    }}
                  >
                    {t("Cancel")}
                  </Button>
                </div>
              </form>
            )}

            {tfaPanel === "regen" && (
              <form className="security-panel" onSubmit={submitRegenerateBackupCodes}>
                <p className="text-secondary text-ink-200">
                  {t("Generating new backup codes invalidates all unused previous codes.")}
                </p>
                {state.hasPassword && (
                  <div className="security-field">
                    <label htmlFor={`${id}-regen-pw`} className="text-label text-ink-300">
                      {t("Current password")}
                    </label>
                    <input
                      id={`${id}-regen-pw`}
                      type="password"
                      autoComplete="current-password"
                      value={tfaPassword}
                      onChange={(e) => setTfaPassword(e.target.value)}
                      className="field w-full"
                      disabled={tfaBusy}
                    />
                  </div>
                )}
                {tfaError && (
                  <p role="alert" className="security-error">
                    {tfaError}
                  </p>
                )}
                <div className="security-actions">
                  <Button type="submit" variant="primary" loading={tfaBusy}>
                    {t("Generate new codes")}
                  </Button>
                  <Button
                    variant="ghost"
                    disabled={tfaBusy}
                    onClick={() => {
                      setTfaPassword("");
                      setTfaError(null);
                      setTfaPanel("none");
                    }}
                  >
                    {t("Cancel")}
                  </Button>
                </div>
              </form>
            )}

            {tfaPanel === "disable" && (
              <form className="security-panel" onSubmit={submitDisableTwoFactor}>
                <p className="text-secondary text-ink-200">
                  {t("Disabling two-factor authentication removes authenticator and backup-code protection from your account.")}
                </p>
                {state.hasPassword && (
                  <div className="security-field">
                    <label htmlFor={`${id}-disable-pw`} className="text-label text-ink-300">
                      {t("Current password")}
                    </label>
                    <input
                      id={`${id}-disable-pw`}
                      type="password"
                      autoComplete="current-password"
                      value={tfaPassword}
                      onChange={(e) => setTfaPassword(e.target.value)}
                      className="field w-full"
                      disabled={tfaBusy}
                    />
                  </div>
                )}
                {tfaError && (
                  <p role="alert" className="security-error">
                    {tfaError}
                  </p>
                )}
                <div className="security-actions">
                  <Button type="submit" variant="danger" loading={tfaBusy}>
                    {t("Disable two-factor authentication")}
                  </Button>
                  <Button
                    variant="ghost"
                    disabled={tfaBusy}
                    onClick={() => {
                      setTfaPassword("");
                      setTfaError(null);
                      setTfaPanel("none");
                    }}
                  >
                    {t("Cancel")}
                  </Button>
                </div>
              </form>
            )}

            {revealedCodes && (
              <div className="security-panel" role="region" aria-label={t("Backup codes")}>
                <div>
                  <h3 className="text-secondary font-semibold text-ink-100">{t("Save your backup codes")}</h3>
                  <p className="mt-1 text-label text-ink-400">
                    {t("Store these one-time recovery codes somewhere safe. Each code can be used once if you lose access to your authenticator app.")}
                  </p>
                </div>
                <ul className="security-backup-grid">
                  {revealedCodes.map((code) => (
                    <li key={code}>{code}</li>
                  ))}
                </ul>
                <div className="security-actions">
                  <Button
                    size="sm"
                    onClick={() => void copyText(revealedCodes.join("\n"), "codes")}
                  >
                    {t(copiedTarget === "codes" ? "Copied" : "Copy codes")}
                  </Button>
                  <Button
                    size="sm"
                    onClick={() =>
                      downloadBackupCodes(
                        revealedCodes,
                        t("Cadence two-factor recovery backup codes"),
                      )
                    }
                  >
                    {t("Download .txt")}
                  </Button>
                  <Button
                    size="sm"
                    variant="primary"
                    onClick={() => {
                      setRevealedCodes(null);
                      setCopiedTarget(null);
                    }}
                  >
                    {t("Done")}
                  </Button>
                </div>
              </div>
            )}
          </section>
        </div>
      )}

      {hosted && policyScope && viewer?.operator === true && !viewer.readOnly && (
        <WorkspaceSecurityPolicy key={policyScope} />
      )}

      <Toast msg={toast} />
    </main>
  );
}
