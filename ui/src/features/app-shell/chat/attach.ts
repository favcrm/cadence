/**
 * CAD-1168: the composer's attach queue. One queued file's lifecycle is
 * pick → uploading → ready|failed; only a `ready` row's daemon-minted id
 * may ride a send. This module holds the pure checks and state updates
 * so Composer stays a thin wrapper and the rules are testable.
 */

export const ATTACH_EXTS = ["txt", "md", "csv"];
/** The file picker's accept list — both densities share it. PDF and
 *  image kinds are deliberately absent: no reviewed processing contract
 *  exists, so they are refused at selection and at the daemon. */
export const ATTACH_ACCEPT = ATTACH_EXTS.map((e) => `.${e}`).join(",");
export const ATTACH_MAX_BYTES = 10 * 1024 * 1024;
export const ATTACH_MAX_FILES = 5;

/** The kinds the ticket names but this interim build cannot process.
 *  A selection of one is refused with this sentence, not silently
 *  dropped and not uploaded as unprocessed bytes. */
export const ATTACH_UNAVAILABLE_EXTS = ["pdf", "png", "jpg", "jpeg", "webp"];

export interface AttachDestination {
  /** Absent for Home; app uploads must always carry their install and conversation. */
  app?: { install_id: string; context_id?: string };
  conversation?: string;
  /** Unsaved-subject preparation captured at selection time. */
  subject?: string;
  prepareConversation?: (stillOwned: () => boolean) => Promise<string>;
  /** Re-read the live descriptor projection at an upload action boundary. */
  refreshCapability?: () => void;
}

export interface AttachItem {
  key: string;
  name: string;
  size: number;
  status: "uploading" | "ready" | "failed";
  file: File;
  id?: string;
  mime?: string;
  error?: string;
  /** A refusal a retry cannot change (kind, size, count, daemon 4xx):
   *  the row offers Remove only. Transient failures keep Retry. */
  permanent?: boolean;
  /** Which upload this row awaits — a remove/re-pick bumps it, so a
   *  late completion lands only on the row that is still its own. */
  generation: number;
  /** Original destination, retained across retry and family/frame moves. */
  destination?: AttachDestination;
}

/** The client's early refusal — the daemon re-checks bytes and the
 *  sniffed kind, so this is a pre-flight hint, never enforcement. */
export function attachRefusal(name: string, size: number): string | null {
  const ext = name.toLowerCase().split(".").pop() ?? "";
  if (ATTACH_UNAVAILABLE_EXTS.includes(ext))
    return `PDF and image processing is not available yet — .${ext} cannot be attached`;
  if (!ATTACH_EXTS.includes(ext))
    return `${ext ? `.${ext}` : "that type"} is not attachable (txt, md, csv)`;
  if (size === 0) return "empty file — nothing to attach";
  if (size > ATTACH_MAX_BYTES) return `over 10 MiB — rejected`;
  return null;
}

/** The daemon words a refusal "chat_file_upload refused 'x.txt': …"; the
 *  operator reads only the reason, never the RPC name or the file again. */
export function userUploadError(message: string): string {
  const reason = message
    .replace(/^chat_file_upload(?: refused)?(?: '[^']*')?:\s*/, "")
    .replace(/ — the interim allowlist is txt, md and csv$/, "")
    .trim();
  return reason.charAt(0).toUpperCase() + reason.slice(1);
}

/** A failed upload is permanent when the server refused it (4xx other
 *  than timeout/rate-limit); network loss, 5xx and aborts can be retried. */
export function isPermanentUploadFailure(status: number | undefined): boolean {
  return status !== undefined && status >= 400 && status < 500 && status !== 408 && status !== 429;
}

/** Size and kind for a file row, e.g. `180 KB · Text`. */
export function attachMeta(name: string, size: number): string {
  const ext = name.toLowerCase().split(".").pop() ?? "";
  const kind = ext === "csv" ? "CSV" : ext === "md" ? "Markdown" : ext === "txt" ? "Text" : ext ? ext.toUpperCase() : "File";
  const kb = size < 1024 ? `${size} B` : size < 1024 * 1024 ? `${Math.round(size / 1024)} KB` : `${(size / 1024 / 1024).toFixed(1)} MB`;
  return `${kb} · ${kind}`;
}

/** Whether `more` files still fit the count cap given `have` queued. */
export function attachFits(have: number, more: number): string | null {
  if (have + more > ATTACH_MAX_FILES) return `at most ${ATTACH_MAX_FILES} attachments per message`;
  return null;
}

/** Whether a ready row can be sent with the currently captured owner. */
export function sameAttachDestination(
  row: AttachDestination | undefined,
  expected: AttachDestination | undefined,
): boolean {
  if (!row || !expected) return row === expected;
  if (
    row.app?.install_id !== expected.app?.install_id ||
    row.app?.context_id !== expected.app?.context_id
  ) return false;
  if (expected.conversation !== undefined) return row.conversation === expected.conversation;
  if (expected.subject !== undefined) return row.subject === expected.subject;
  return row.conversation === undefined && row.subject === undefined;
}

/** A row still doing work or still unresolved (failed rows stay until
 *  the operator retries or removes them — send is gated on these so a
 *  queued file is never silently dropped from its message). */
export function unresolved(items: AttachItem[]): AttachItem[] {
  return items.filter((i) => i.status !== "ready");
}
/** The submit-time sentence for `title`/aria when a send is withheld
 *  because rows are unresolved — specific over a generic disabled. */
export function unresolvedHint(items: AttachItem[]): string | null {
  const bad = unresolved(items);
  if (bad.length === 0) return null;
  const uploading = bad.filter((i) => i.status === "uploading").length;
  const failed = bad.filter((i) => i.status === "failed");
  const reasons = [...new Set(failed.map((i) => i.error).filter(Boolean))].join("; ");
  const parts = [
    uploading > 0 ? `${uploading} file${uploading === 1 ? " is" : "s are"} still uploading` : "",
    failed.length > 0 ? `${failed.length} file${failed.length === 1 ? "" : "s"} failed${reasons ? ` — ${reasons}` : ""}` : "",
  ].filter(Boolean);
  return `${parts.join(" · ")} — resolve or remove before sending`;
}
