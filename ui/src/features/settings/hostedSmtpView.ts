import { ApiError } from "../../lib/api";

/**
 * Pure rules for hosted "Connect your email (SMTP)" (CAD-1126). A hosted
 * workspace is offline; the daemon sends through the platform's
 * `smtp.internal` pass-through. This module owns the presets and the
 * plain-words reading of the daemon's typed `smtp_*` refusals. The
 * board's own message text is never rendered: only the code is read.
 */

export type TlsMode = "implicit" | "starttls";

export interface Preset {
  id: "gmail" | "microsoft" | "other";
  label: string;
  host: string;
  port: 465 | 587;
  tls: TlsMode;
  hint: string;
}

export const PRESETS: readonly Preset[] = [
  {
    id: "gmail",
    label: "Gmail / Google Workspace",
    host: "smtp.gmail.com",
    port: 465,
    tls: "implicit",
    hint: "Use an app password, not your normal password: turn on 2-Step Verification, then create one at myaccount.google.com/apppasswords.",
  },
  {
    id: "microsoft",
    label: "Microsoft 365",
    host: "smtp.office365.com",
    port: 587,
    tls: "starttls",
    hint: "Use your Microsoft 365 address. If your account asks for one, use an app password. SMTP sending must be on for the mailbox.",
  },
  {
    id: "other",
    label: "Other",
    host: "",
    port: 587,
    tls: "starttls",
    hint: "Use your provider's SMTP server with port 465 (SSL/TLS) or 587 (STARTTLS).",
  },
];

/** The reviewed send scope an SMTP sender enrolls (src/platform/smtp.rs). */
export const SMTP_SEND_SCOPE = "email:send";
/** The one account name the hosted Settings flow manages. */
export const HOSTED_ACCOUNT = "email";

const WORDS: Record<string, string> = {
  smtp_auth: "Couldn't sign in. Check the app password and the username.",
  smtp_tls: "Couldn't make a secure connection. Check the port and the security setting.",
  smtp_connect: "Couldn't reach the mail server. Check the server name and port.",
  smtp_dns: "That mail server name wasn't found. Check the spelling.",
  smtp_private_host: "That isn't a public mail server. Use your provider's server name.",
  smtp_port: "Use port 465 (SSL/TLS) or 587 (STARTTLS).",
  smtp_timeout: "The mail server took too long to answer. Try again.",
  smtp_rate: "Too many emails in a short time. Wait a little and try again.",
  smtp_not_provisioned: "Email sending isn't set up for this workspace yet.",
  smtp_invalid: "Some details aren't valid. Check each field.",
  smtp_unreachable: "The mail relay isn't reachable right now. Try again in a moment.",
  smtp_failed: "The mail server refused the connection. Check the details and try again.",
  smtp_unknown: "The mail server refused the connection. Check the details and try again.",
  custody_unprotected:
    "This workspace couldn't store the password safely without your consent. Tick the box and try again.",
};

/** Plain words for a refusal; never the raw message, which can echo input. */
export function connectErrorText(error: unknown): string {
  const code = error instanceof ApiError ? error.code : undefined;
  if (code !== undefined && code in WORDS) return WORDS[code];
  return "Couldn't confirm the connection. Refresh and check before trying again.";
}

/** Security follows the port: 465 is SSL/TLS, 587 is STARTTLS. */
export function portFor(tls: TlsMode): 465 | 587 {
  return tls === "implicit" ? 465 : 587;
}
