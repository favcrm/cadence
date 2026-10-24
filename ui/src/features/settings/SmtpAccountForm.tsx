import { useEffect, useRef, useState } from "react";
import { api } from "../../lib/api";
import type { Connection } from "../../lib/types";
import Button from "../../ui/Button";
import HostedSmtpConnect, { type ConnectDetails } from "./HostedSmtpConnect";
import { HOSTED_ACCOUNT, SMTP_SEND_SCOPE } from "./hostedSmtpView";
import { useTranslate } from "../../lib/locale";

/**
 * CAD-1797: the email account lives in Connections. The tenant board does
 * not carry the provider catalogue, so this form is the SMTP account
 * surface there; the local daemon reaches the same account through its
 * provider setup. It stores one workspace email account: add when absent,
 * replace when present. Apps choose the account in Email sending.
 */
export default function SmtpAccountForm({
  rows,
  onSaved,
}: {
  rows: Connection[];
  onSaved: (id: string) => void;
}) {
  const t = useTranslate();
  const emailRow =
    rows.find((row) => row.provider === "smtp" && row.account === HOSTED_ACCOUNT) ??
    rows.find((row) => row.provider === "smtp") ??
    null;
  const [replacing, setReplacing] = useState(false);
  const mounted = useRef(true);
  const request = useRef(0);
  const address = emailRow?.smtp?.sender ?? null;

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      request.current += 1;
    };
  }, []);

  const connect = async (d: ConnectDetails, acceptRisk: boolean) => {
    const generation = ++request.current;
    const fields = {
      host: d.host,
      port: d.port,
      tls_mode: d.tls_mode,
      username: d.username,
      secret: d.password,
      sender: d.sender,
      ...(d.sender_name ? { sender_name: d.sender_name } : {}),
      // Only after the daemon refused with custody_unprotected and the
      // operator ticked the honest consent (CAD-1013 pattern).
      ...(acceptRisk ? { accept_same_uid_risk: true } : {}),
    };
    const out = emailRow
      ? await api.connectionRotate(emailRow.id, fields)
      : await api.connectionCreate({
          provider: "smtp",
          account: HOSTED_ACCOUNT,
          shape: "smtp",
          scopes: [SMTP_SEND_SCOPE],
          ...fields,
        });
    // A route change or Cancel while verification is in flight must not
    // pull the operator off the page they moved to.
    if (!mounted.current || generation !== request.current) return;
    onSaved(out.connection.id);
  };

  if (emailRow && !replacing) {
    return (
      <div className="grid gap-3 min-w-0" data-section="email-account">
        <p className="text-label text-ink-300 break-words" data-state="email-connected">
          {t("Connected: {account}").replace("{account}", () => String(address ?? emailRow.account))}
        </p>
        <div className="crm-toolbar">
          <Button size="sm" onClick={() => { request.current += 1; setReplacing(true); }}>{t("Replace email account")}</Button>
        </div>
        <p className="text-micro text-ink-500 break-words">
          {t("Apps choose this account in Settings → Email sending.")}
        </p>
      </div>
    );
  }

  return (
    <div className="grid gap-3 min-w-0" data-section="email-account">
      <p className="text-label text-ink-400 break-words">
        {t("This is the workspace email account. Apps can use it or another account you add here; each app chooses its sender in Settings → Email sending.")}
      </p>
      <HostedSmtpConnect
        connected={address}
        onConnect={connect}
        onCancel={emailRow ? () => { request.current += 1; setReplacing(false); } : undefined}
      />
    </div>
  );
}
