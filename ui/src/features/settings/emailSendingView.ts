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

/** What the daemon says about the hosted platform sender (CAD-1121). */
export interface HostedSender {
  hosted: boolean;
  available: boolean;
  missing: string | null;
}

export function hostedSenderFrom(payload: {
  hosted?: boolean;
  platform_sender?: { available: boolean; missing: string | null };
}): HostedSender {
  return {
    hosted: payload.hosted === true,
    available: payload.platform_sender?.available === true,
    missing: payload.platform_sender?.missing ?? null,
  };
}

/** Plain copy when a hosted workspace has no platform sender yet. */
export const PLATFORM_EMAIL_WAITING = "Platform email isn't set up for this workspace yet";
export const PLATFORM_EMAIL_WAITING_NOTE =
  "This is waiting on the platform, not something to fix in the CRM.";
/** The one line shown beside raw-SMTP connections on a hosted workspace. */
export const HOSTED_SMTP_NOTE = "Hosted workspaces send through the platform; SMTP is for self-hosted Cadence.";

export function senderChoices(rows: Connection[], hosted = false): SenderChoices {
  // CAD-1121: on a hosted daemon the platform sender is the only choice.
  if (hosted) return { usable: rows.filter((r) => isHostedSender(r)), unusable: [] };
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
  binding: { state: string; usable?: boolean; sender: { address: string } } | null | undefined,
): string | null {
  return binding && binding.state === "live" && binding.usable !== false ? binding.sender.address : null;
}

/** Where campaigns send the operator to choose a sender. */
export const EMAIL_SENDING_HREF = "/settings/email";

/** What a hosted send parked for the owner reads as — never an error. */
export const WAITING_APPROVAL_TEXT = "Waiting for owner approval in AgenticOS";

/** The daemon's typed reason on a queued delivery awaiting the owner. */
export function waitingApproval(row: { state: string; reason: string | null }): boolean {
  return row.state === "queued" && (row.reason ?? "").toLowerCase().startsWith("waiting for owner approval");
}
