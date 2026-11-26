import { useState } from "react";
import { ApiError } from "../../lib/api";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import {
  PRESETS,
  connectErrorText,
  portFor,
  type Preset,
  type TlsMode,
} from "./hostedSmtpView";
import { useTranslate } from "../../lib/locale";

/**
 * Hosted "Connect your email (SMTP)" (CAD-1126): the primary way a hosted
 * workspace sends campaign email. The form hands one request to
 * `onConnect`; the password lives in this component's state only until
 * that request settles and is cleared whatever the outcome.
 */

export interface ConnectDetails {
  host: string;
  port: number;
  tls_mode: TlsMode;
  username: string;
  password: string;
  sender: string;
  sender_name: string;
}

// Stored validation keys: English kept in state, translated at render.
const UI_MISSING_FIELDS = "Fill in your email address, the app password and the mail server.";
const UI_CONSENT_NEEDED = "Tick the box to store the password anyway, or cancel.";

export default function HostedSmtpConnect({
  connected,
  onConnect,
  onCancel,
  accountLabel,
  onAccountLabelChange,
}: {
  /** The address already connected, if any: the form then replaces it. */
  connected: string | null;
  onConnect: (details: ConnectDetails, acceptRisk: boolean) => Promise<void>;
  onCancel?: () => void;
  accountLabel?: string;
  onAccountLabelChange?: (value: string) => void;
}) {
  const t = useTranslate();
  const [preset, setPreset] = useState<Preset>(PRESETS[0]);
  const [host, setHost] = useState(PRESETS[0].host);
  const [tls, setTls] = useState<TlsMode>(PRESETS[0].tls);
  const [address, setAddress] = useState(connected ?? "");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [name, setName] = useState("");
  const [consent, setConsent] = useState(false);
  // Set only when the daemon refused with custody_unprotected.
  const [riskNeeded, setRiskNeeded] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const choose = (next: Preset) => {
    setPreset(next);
    setHost(next.host);
    setTls(next.tls);
    setPassword("");
    setConsent(false);
    setRiskNeeded(false);
    setError(null);
  };

  const submit = () => {
    if (pending) return;
    if (onAccountLabelChange && !accountLabel?.trim()) {
      setError("Name this email account.");
      return;
    }
    if (address.trim() === "" || password === "" || host.trim() === "") {
      setError(UI_MISSING_FIELDS);
      return;
    }
    if (riskNeeded && !consent) {
      setError(UI_CONSENT_NEEDED);
      return;
    }
    setPending(true);
    setError(null);
    void onConnect({
      host: host.trim(),
      port: portFor(tls),
      tls_mode: tls,
      username: username.trim() || address.trim(),
      password,
      sender: address.trim(),
      sender_name: name.trim(),
    }, riskNeeded && consent)
      .then(() => setPassword(""))
      .catch((e: unknown) => {
        // A refusal stores nothing; never keep the secret after the request
        // settles, including the custody-consent refusal. The operator
        // re-enters the password after ticking consent.
        setPassword("");
        if (e instanceof ApiError && e.code === "custody_unprotected") {
          setRiskNeeded(true);
        }
        setError(connectErrorText(e));
      })
      .finally(() => setPending(false));
  };

  return (
    <form
      className="grid gap-3 min-w-0"
      aria-label={t("Connect your email")}
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      <div className="crm-field">
        <label className="text-label text-ink-300" htmlFor="hs-preset">{t("Email provider")}</label>
        <Select id="hs-preset" full value={preset.id} disabled={pending}
          options={PRESETS.map((p) => ({ value: p.id, label: t(p.label) }))}
          onChange={(value) => { const next = PRESETS.find((p) => p.id === value); if (next) choose(next); }} />
      </div>
      <p className="text-label text-ink-400 break-words" data-hint={preset.id}>
        {t(preset.hint)}
      </p>
      <h3 className="text-cardtitle font-medium text-ink-100 mt-2">{t("Sender details")}</h3>
      <div>
        <div className="crm-field">
          <label className="text-label text-ink-300" htmlFor="hs-address">{t("Your email address")}</label>
          <input
            id="hs-address"
            className="field"
            type="email"
            autoComplete="off"
            spellCheck={false}
            value={address}
            onChange={(e) => setAddress(e.target.value)}
            placeholder="you@yourcompany.com"
            disabled={pending}
          />
        </div>
      </div>
      {onAccountLabelChange && (
        <div className="crm-field">
          <label className="text-label text-ink-300" htmlFor="hs-account">{t("Account label")}</label>
          <input id="hs-account" className="field" value={accountLabel ?? ""} onChange={(e) => onAccountLabelChange(e.target.value)} disabled={pending} />
        </div>
      )}
      <div className="crm-field">
        <label className="text-label text-ink-300" htmlFor="hs-name">{t("From name (optional)")}</label>
        <input
          id="hs-name"
          className="field"
          autoComplete="off"
          value={name}
          onChange={(e) => setName(e.target.value)}
          maxLength={80}
          placeholder={t("Your restaurant")}
          disabled={pending}
        />
      </div>
      <h3 className="text-cardtitle font-medium text-ink-100 mt-2">{t("Server settings")}</h3>
        <div className="crm-field-row">
          <div className="crm-field">
            <label className="text-label text-ink-300" htmlFor="hs-host">{t("Mail server")}</label>
            <input
              id="hs-host"
              className="field"
              autoComplete="off"
              spellCheck={false}
              value={host}
              onChange={(e) => setHost(e.target.value)}
              placeholder="mail.example.com"
              disabled={pending}
            />
          </div>
          <div className="crm-field">
            <label className="text-label text-ink-300" htmlFor="hs-security">{t("Port and security")}</label>
            <Select id="hs-security" full value={tls} disabled={pending}
              options={[{ value: "implicit", label: t("465 · SSL/TLS") }, { value: "starttls", label: t("587 · STARTTLS") }]}
              onChange={(value) => setTls(value as TlsMode)} />
          </div>
        </div>
      <div className="crm-field">
        <label className="text-label text-ink-300" htmlFor="hs-username">{t("SMTP username (optional)")}</label>
        <input id="hs-username" className="field" autoComplete="username" spellCheck={false} value={username} onChange={(e) => setUsername(e.target.value)} disabled={pending} />
        <p className="text-micro text-ink-500">{t("Leave blank to use your email address.")}</p>
      </div>
      <div className="crm-field">
        <label className="text-label text-ink-300" htmlFor="hs-password">{preset.id === "other" ? t("SMTP password") : t("App password")}</label>
        <input id="hs-password" className="field" type="password" autoComplete="current-password" spellCheck={false}
          value={password} onChange={(e) => setPassword(e.target.value)} disabled={pending} />
      </div>
      {riskNeeded && (
        <label className="flex items-start gap-2 text-label text-ink-300" data-consent>
          <input
            type="checkbox"
            checked={consent}
            onChange={(e) => setConsent(e.target.checked)}
            disabled={pending}
            className="mt-0.5"
          />
          <span>{t("This workspace can't keep the password apart from agents running under the same system account, so they could read it. I accept storing it here.")}</span>
        </label>
      )}
      {error && (
        <p className="text-label text-fail break-words" role="alert">
          {t(error)}
        </p>
      )}
      <div className="crm-toolbar">
        <Button type="submit" size="sm" variant="primary" loading={pending} disabled={pending}>
          {connected ? t("Replace email account") : t("Connect email")}
        </Button>
        {onCancel && (
          <Button size="sm" disabled={pending} onClick={onCancel}>{t("Cancel")}</Button>
        )}
      </div>
    </form>
  );
}
