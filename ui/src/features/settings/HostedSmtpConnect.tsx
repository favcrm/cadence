import { useState } from "react";
import { ApiError } from "../../lib/api";
import Button from "../../ui/Button";
import {
  PRESETS,
  connectErrorText,
  portFor,
  type Preset,
  type TlsMode,
} from "./hostedSmtpView";

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

export default function HostedSmtpConnect({
  connected,
  onConnect,
  onCancel,
}: {
  /** The address already connected, if any: the form then replaces it. */
  connected: string | null;
  onConnect: (details: ConnectDetails, acceptRisk: boolean) => Promise<void>;
  onCancel?: () => void;
}) {
  const [preset, setPreset] = useState<Preset>(PRESETS[0]);
  const [host, setHost] = useState(PRESETS[0].host);
  const [tls, setTls] = useState<TlsMode>(PRESETS[0].tls);
  const [address, setAddress] = useState("");
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
    setError(null);
  };

  const submit = () => {
    if (pending) return;
    if (address.trim() === "" || password === "" || host.trim() === "") {
      setError("Fill in your email address, the app password and the mail server.");
      return;
    }
    if (riskNeeded && !consent) {
      setError("Tick the box to store the password anyway, or cancel.");
      return;
    }
    setPending(true);
    setError(null);
    void onConnect({
      host: host.trim(),
      port: portFor(tls),
      tls_mode: tls,
      username: address.trim(),
      password,
      sender: address.trim(),
      sender_name: name.trim(),
    }, riskNeeded && consent)
      .then(() => setPassword(""))
      .catch((e: unknown) => {
        // Nothing was stored on this refusal; keep the typed password
        // so the operator can tick and resend without retyping it.
        if (e instanceof ApiError && e.code === "custody_unprotected") {
          setRiskNeeded(true);
          setError(connectErrorText(e));
          return;
        }
        setPassword("");
        setError(connectErrorText(e));
      })
      .finally(() => setPending(false));
  };

  return (
    <form
      className="grid gap-3 min-w-0"
      aria-label="Connect your email"
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      <div role="group" aria-label="Email provider" className="crm-toolbar">
        {PRESETS.map((p) => (
          <Button
            key={p.id}
            size="sm"
            variant={p.id === preset.id ? "primary" : "secondary"}
            aria-pressed={p.id === preset.id}
            disabled={pending}
            onClick={() => choose(p)}
          >
            {p.label}
          </Button>
        ))}
      </div>
      <p className="text-label text-ink-400 break-words" data-hint={preset.id}>
        {preset.hint}
      </p>
      <div className="crm-field-row">
        <div className="crm-field">
          <label className="text-label text-ink-300" htmlFor="hs-address">
            Your email address
          </label>
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
        <div className="crm-field">
          <label className="text-label text-ink-300" htmlFor="hs-password">
            App password
          </label>
          <input
            id="hs-password"
            className="field"
            type="password"
            autoComplete="off"
            spellCheck={false}
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            disabled={pending}
          />
        </div>
      </div>
      <div className="crm-field">
        <label className="text-label text-ink-300" htmlFor="hs-name">
          From name (optional)
        </label>
        <input
          id="hs-name"
          className="field"
          autoComplete="off"
          value={name}
          onChange={(e) => setName(e.target.value)}
          maxLength={80}
          placeholder="Your restaurant"
          disabled={pending}
        />
      </div>
      {preset.id === "other" && (
        <div className="crm-field-row">
          <div className="crm-field">
            <label className="text-label text-ink-300" htmlFor="hs-host">
              Mail server
            </label>
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
            <label className="text-label text-ink-300" htmlFor="hs-security">
              Port and security
            </label>
            <select
              id="hs-security"
              className="field"
              value={tls}
              onChange={(e) => setTls(e.target.value as TlsMode)}
              disabled={pending}
            >
              <option value="implicit">465 · SSL/TLS</option>
              <option value="starttls">587 · STARTTLS</option>
            </select>
          </div>
        </div>
      )}
      {preset.id !== "other" && (
        <p className="text-micro text-ink-500 num" data-server>
          {host} · port {portFor(tls)} · {tls === "implicit" ? "SSL/TLS" : "STARTTLS"}
        </p>
      )}
      {riskNeeded && (
        <label className="flex items-start gap-2 text-label text-ink-300" data-consent>
          <input
            type="checkbox"
            checked={consent}
            onChange={(e) => setConsent(e.target.checked)}
            disabled={pending}
            className="mt-0.5"
          />
          <span>
            This workspace can't keep the password apart from agents running under the same
            system account, so they could read it. I accept storing it here.
          </span>
        </label>
      )}
      {error && (
        <p className="text-label text-fail break-words" role="alert">
          {error}
        </p>
      )}
      <div className="crm-toolbar">
        <Button type="submit" size="sm" variant="primary" loading={pending} disabled={pending}>
          {connected ? "Replace email account" : "Connect email"}
        </Button>
        {onCancel && (
          <Button size="sm" disabled={pending} onClick={onCancel}>
            Cancel
          </Button>
        )}
      </div>
    </form>
  );
}
