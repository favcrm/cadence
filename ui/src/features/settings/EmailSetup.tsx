import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { api } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import type { Connection, ConnectionProvider } from "../../lib/types";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import Select from "../../ui/Select";
import { useProtectedEntry } from "../connections/useProtectedEntry";
import { enrollmentSupport } from "./connectionsView";
import HostedSmtpConnect, { type ConnectDetails } from "./HostedSmtpConnect";
import { HOSTED_ACCOUNT, SMTP_SEND_SCOPE } from "./hostedSmtpView";
import SettingsDrawer from "./SettingsDrawer";

export default function EmailSetup({ rows, hostedSmtp, onSaved, onClose }: {
  rows: Connection[];
  hostedSmtp: boolean;
  onSaved: () => void;
  onClose: () => void;
}) {
  const t = useTranslate();
  const [step, setStep] = useState<1 | 2>(1);
  const [method, setMethod] = useState("smtp");
  const [account, setAccount] = useState("");
  const [provider, setProvider] = useState<ConnectionProvider | null>(null);
  const [catalog, setCatalog] = useState<"loading" | "ready" | "failed">(hostedSmtp ? "ready" : "loading");
  const [pending, setPending] = useState(false);
  const live = useRef(true);
  const generation = useRef(0);
  const heading = useRef<HTMLHeadingElement>(null);
  const opener = useRef(typeof document === "undefined" ? null : document.activeElement);
  const focusMethod = useRef(false);

  useLayoutEffect(() => {
    live.current = true;
    return () => { live.current = false; generation.current += 1; };
  }, []);
  useLayoutEffect(() => {
    if (step === 1 && focusMethod.current) {
      focusMethod.current = false;
      document.getElementById("email-method")?.focus();
    }
  }, [step]);
  useEffect(() => {
    if (hostedSmtp) return;
    let active = true;
    api.connectionProviders().then(({ providers }) => {
      if (!active) return;
      setProvider(providers.find((p) => p.provider === "smtp") ?? null);
      setCatalog("ready");
    }).catch(() => { if (active) setCatalog("failed"); });
    return () => { active = false; };
  }, [hostedSmtp]);

  const scopes = [...new Set((provider?.descriptor?.capabilities ?? [])
    .filter((cap) => cap.effect === "send").flatMap((cap) => cap.scopes ?? []))].sort();
  const nativeSupported = provider !== null && enrollmentSupport(provider).shapes.includes("smtp") && scopes.length > 0;
  const alreadyHosted = hostedSmtp && rows.some((row) => row.provider === "smtp" && row.account === HOSTED_ACCOUNT);
  const supported = (hostedSmtp || nativeSupported) && !alreadyHosted;

  const connect = async (details: ConnectDetails, acceptRisk: boolean) => {
    const request = ++generation.current;
    const name = hostedSmtp ? HOSTED_ACCOUNT : account.trim();
    if (hostedSmtp && rows.some((row) => row.provider === "smtp" && row.account === HOSTED_ACCOUNT)) {
      throw new Error("Account already exists");
    }
    setPending(true);
    try {
      await api.connectionCreate({
        provider: "smtp", account: name, shape: "smtp",
        scopes: hostedSmtp ? [SMTP_SEND_SCOPE] : scopes,
        host: details.host, port: details.port, tls_mode: details.tls_mode,
        username: details.username, secret: details.password, sender: details.sender,
        ...(details.sender_name ? { sender_name: details.sender_name } : {}),
        ...(acceptRisk ? { accept_same_uid_risk: true } : {}),
      });
      if (live.current && generation.current === request) onSaved();
    } finally {
      if (live.current) setPending(false);
    }
  };

  const cancel = () => {
    onClose();
    window.setTimeout(() => {
      if (opener.current instanceof HTMLElement && opener.current.isConnected) opener.current.focus();
    }, 0);
  };

  return (
    <SettingsDrawer title={t("Add email")} onClose={onClose} preventClose={pending}>
      <div className="grid gap-4 min-w-0">
        <div>
          <p className="text-micro text-ink-500">{step === 1 ? t("Email · Step 1 of 2") : t("Email · Step 2 of 2")}</p>
          <div className="flex gap-1 mt-2" aria-hidden="true">
            <span className="h-1 flex-1 rounded bg-accent" />
            <span className={`h-1 flex-1 rounded ${step === 2 ? "bg-accent" : "bg-ink-700"}`} />
          </div>
        </div>
        {step === 1 ? (
          <>
            <p className="text-label text-ink-400">{t("Connect a sending account for this workspace.")}</p>
            <div className="crm-field">
              <label htmlFor="email-method" className="text-label text-ink-300">{t("Connection method")}</label>
              <Select id="email-method" full value={method} onChange={setMethod} options={[
                { value: "smtp", label: t("SMTP server"), hint: t("Use your provider’s SMTP settings") },
                { value: "google", label: t("Gmail"), badge: t("Not available"), disabled: true },
                { value: "microsoft", label: t("Outlook / Microsoft 365"), badge: t("Not available"), disabled: true },
              ]} />
            </div>
            <div className="rounded border border-ink-700 bg-ink-850 p-4 grid gap-1">
              <div className="flex flex-wrap justify-between gap-2"><strong className="text-label text-ink-100">{t("Use your provider’s SMTP settings")}</strong><span className="text-micro text-ink-400">{catalog === "loading" ? t("Checking availability…") : supported ? t("Supported") : t("Unavailable")}</span></div>
              <p className="text-label text-ink-400">{t("Start with a preset or enter the server details yourself.")}</p>
            </div>
            {alreadyHosted && <p className="text-label text-ink-400">{t("The workspace email account already exists. Manage it to replace its credentials.")}</p>}
            {!hostedSmtp && catalog === "failed" && <p className="text-label text-fail" role="alert">{t("Email setup availability could not be loaded.")}</p>}
            {!hostedSmtp && catalog === "ready" && !nativeSupported && <p className="text-label text-ink-400">{t("SMTP sending is not available from a reviewed provider on this board.")}</p>}
            <p className="text-label text-ink-400 rounded border border-ink-700 p-3">{t("Adding an account doesn’t change any app’s sender.")}</p>
            <div className="crm-toolbar justify-end">
              <Button onClick={cancel}>{t("Cancel")}</Button>
              <Button variant="primary" disabled={!supported} onClick={() => { setStep(2); requestAnimationFrame(() => heading.current?.focus()); }}>{t("Continue")}</Button>
            </div>
          </>
        ) : (
          <>
            <h2 ref={heading} tabIndex={-1} className="text-cardtitle font-medium text-ink-100">{t("SMTP configuration")}</h2>
            {hostedSmtp
              ? <HostedSmtpConnect connected={null} onConnect={connect} />
              : <NativeSmtpEntry account={account} onAccountChange={setAccount} onConnect={connect} />}
            <div className="crm-toolbar"><Button disabled={pending} onClick={() => { focusMethod.current = true; setStep(1); }}>{t("Back")}</Button><Button disabled={pending} onClick={cancel}>{t("Cancel")}</Button></div>
          </>
        )}
      </div>
    </SettingsDrawer>
  );
}

function NativeSmtpEntry({ account, onAccountChange, onConnect }: {
  account: string;
  onAccountChange: (value: string) => void;
  onConnect: (details: ConnectDetails, acceptRisk: boolean) => Promise<void>;
}) {
  const t = useTranslate();
  const entry = useProtectedEntry();
  if (entry.status === "loading") return <p role="status" className="text-label text-ink-400">{t("Checking credential entry…")}</p>;
  if (entry.status === "ready" && entry.entry?.configured === true && entry.entry.protected === true) {
    return <Link href="/settings/connections/new/smtp" className="lnk text-label">{t("Continue to protected setup")}</Link>;
  }
  if (entry.status !== "ready" || entry.entry?.configured !== false || entry.entry.protected !== false || entry.entry.origin !== null) {
    return <p role="alert" className="text-label text-fail">{t("Credential entry could not be checked. No password can be entered here.")}</p>;
  }
  return <HostedSmtpConnect connected={null} onConnect={onConnect} accountLabel={account} onAccountLabelChange={onAccountChange} />;
}
