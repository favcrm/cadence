/**
 * CAD-1168: the composer's attach queue. One queued file's lifecycle is
 * pick → uploading → ready|failed; only a `ready` row's daemon-minted id
 * may ride a send. This module holds the pure checks and state updates
 * so Composer stays a thin wrapper and the rules are testable.
 */

type Translate = (english: string) => string;
const identity: Translate = (english) => english;
export const ATTACH_MAX_BYTES = 10 * 1024 * 1024;
export const ATTACH_MAX_FILES = 5;

export interface AttachDestination {
  /** Absent for Home; app uploads must carry their install and conversation. */
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
  /** A refusal a retry cannot change (size, count, daemon 4xx):
   *  the row offers Remove only. Transient failures keep Retry. */
  permanent?: boolean;
  /** Which upload this row awaits — a remove/re-pick bumps it, so a
   *  late completion lands only on the row that is still its own. */
  generation: number;
  /** Original destination, retained across retry and family/frame moves. */
  destination?: AttachDestination;
}

/** The client checks the size; the server decides whether the bytes are usable. */
export function attachRefusal(size: number): string | null {
  if (size === 0) return "empty file — nothing to attach";
  if (size > ATTACH_MAX_BYTES) return `over 10 MiB — rejected`;
  return null;
}

/** Translate local validation messages, including the dynamic attachment count. */
export function displayAttachError(message: string, t: Translate = identity): string {
  const count = /^at most (\d+) attachments per message$/.exec(message);
  if (count)
    return t("at most {count} attachments per message").replace("{count}", () => count[1]);
  return t(message);
}

export function userUploadError(message: string): string {
  const reason = message
    .replace(/^chat_file_upload(?: refused)?(?: '[^']*')?:\s*/, "")
    .trim();
  return reason.charAt(0).toUpperCase() + reason.slice(1);
}

/** A failed upload is permanent when the server refused it (4xx other
 *  than timeout/rate-limit); network loss, 5xx and aborts can be retried. */
export function isPermanentUploadFailure(status: number | undefined): boolean {
  return status !== undefined && status >= 400 && status < 500 && status !== 408 && status !== 429;
}

/** The kind label for a row's meta line; other kinds fall back to their extension. */
const KIND_LABELS: Record<string, string> = {
  csv: "CSV",
  tsv: "TSV",
  json: "JSON",
  jsonl: "JSONL",
  yaml: "YAML",
  yml: "YAML",
  xml: "XML",
  pdf: "PDF",
};

/** Size and kind for a file row, e.g. `180 KB · Text`. */
export function attachMeta(name: string, size: number, t: Translate = identity): string {
  const ext = name.toLowerCase().split(".").pop() ?? "";
  const kind =
    KIND_LABELS[ext] ??
    (ext === "md" || ext === "markdown"
      ? t("Markdown")
      : ext === "txt"
        ? t("Text")
        : ext
          ? ext.toUpperCase()
          : t("File"));
  const kb = size < 1024 ? `${size} B` : size < 1024 * 1024 ? `${Math.round(size / 1024)} KB` : `${(size / 1024 / 1024).toFixed(1)} MB`;
  return `${kb} · ${kind}`;
}

/** Whether `more` files still fit the count cap given `have` queued. */
export function attachFits(have: number, more: number, t: Translate = identity): string | null {
  if (have + more > ATTACH_MAX_FILES)
    return t("at most {count} attachments per message").replace("{count}", String(ATTACH_MAX_FILES));
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
/** The send-hold notice as a locale-neutral UiNotice: a whole-sentence
 *  catalog key plus raw slots. Nothing is translated here, so a stored
 *  notice follows the locale active at render. Null when nothing is
 *  unresolved. The reason text is the last slot, so no later slot
 *  replacement can match a brace inside it. */
export function unresolvedHintNotice(items: AttachItem[]): { key: string; slots: Record<string, string> } | null {
  const bad = unresolved(items);
  if (bad.length === 0) return null;
  const uploading = bad.filter((i) => i.status === "uploading").length;
  const failed = bad.filter((i) => i.status === "failed");
  const reasons = [...new Set(failed.map((i) => i.error).filter(Boolean))].join("; ");
  const reasonSuffix = reasons ? ` — ${reasons}` : "";
  if (failed.length === 0)
    return {
      key: uploading === 1
        ? "{count} file is still uploading — resolve or remove before sending"
        : "{count} files are still uploading — resolve or remove before sending",
      slots: { count: String(uploading) },
    };
  if (uploading === 0)
    return {
      key: failed.length === 1
        ? "{count} file failed{reasonSuffix} — resolve or remove before sending"
        : "{count} files failed{reasonSuffix} — resolve or remove before sending",
      slots: { count: String(failed.length), reasonSuffix },
    };
  return {
    key: uploading === 1
      ? failed.length === 1
        ? "{uploading} file is still uploading · {failed} file failed{reasonSuffix} — resolve or remove before sending"
        : "{uploading} file is still uploading · {failed} files failed{reasonSuffix} — resolve or remove before sending"
      : failed.length === 1
        ? "{uploading} files are still uploading · {failed} file failed{reasonSuffix} — resolve or remove before sending"
        : "{uploading} files are still uploading · {failed} files failed{reasonSuffix} — resolve or remove before sending",
    slots: { uploading: String(uploading), failed: String(failed.length), reasonSuffix },
  };
}

/** The English-or-translated hold sentence for `title`/aria. Expands the
 *  same notice as the stored path: translate the whole key, then insert
 *  each slot literally. */
export function unresolvedHint(items: AttachItem[], t: Translate = identity): string | null {
  const notice = unresolvedHintNotice(items);
  if (notice === null) return null;
  let text = t(notice.key);
  for (const [name, value] of Object.entries(notice.slots)) text = text.replace(`{${name}}`, () => value);
  return text;
}
