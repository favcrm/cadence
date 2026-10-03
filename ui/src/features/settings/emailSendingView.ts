import type { Connection } from "../../lib/types";
import { isSmtpSender, smtpErrorMessage } from "./connectionsView";

/**
 * Pure view rules for Settings → Email sending (CAD-1059). The sender
 * list is the Connections list narrowed to rows that can send CRM mail:
 * enrolled SMTP senders and the built-in hosted AgenticOS sender.
 * Reads metadata only — nothing here sees a secret.
 */

/** The hosted AgenticOS sender: its smtp projection is marked `platform`. */
export function isHostedSender(row: Pick<Connection, "smtp">): boolean {
  return row.smtp?.tls_mode === "platform";
}

/** True when the bound sender is the platform (hosted) sender. */
export function isHostedTransport(binding: { transportKind: string } | null | undefined): boolean {
  return binding?.transportKind === "agenticos";
}

/** The one sentence a sender is named by. */
export function senderLine(row: Connection): string {
  const smtp = row.smtp;
  if (!smtp) return `${row.provider} · ${row.account}`;
  if (isHostedSender(row)) return `Sending via AgenticOS — ${smtp.sender}`;
  const mode = smtp.tls_mode === "implicit" ? "implicit TLS" : "STARTTLS";
  return `${smtp.sender} · ${smtp.host}:${smtp.port} · ${mode} · login ${smtp.username}`;
}

export interface SenderChoices {
  usable: Connection[];
  /** SMTP senders that exist but cannot send, each with its typed reason. */
  unusable: { row: Connection; reason: string }[];
}

export function senderChoices(rows: Connection[]): SenderChoices {
  const senders = rows.filter(isSmtpSender);
  const usable: Connection[] = [];
  const unusable: SenderChoices["unusable"] = [];
  for (const row of senders) {
    if ((row.smtp ?? null) !== null && row.status.custody_available === true) usable.push(row);
    else
      unusable.push({
        row,
        reason:
          smtpErrorMessage(row) ??
          "Its credential is not available. Rotate it to re-enter its settings.",
      });
  }
  return { usable, unusable };
}

/** "Sending from <address>" for a live binding; null otherwise. */
export function sendingFrom(
  binding: { state: string; sender: { address: string } } | null | undefined,
): string | null {
  return binding && binding.state === "live" ? binding.sender.address : null;
}
