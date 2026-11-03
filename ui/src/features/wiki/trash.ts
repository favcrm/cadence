import type { TrashEntry, TrashLanding, WikiKind } from "./api";
import { safeTrashFrom } from "./wire";

/**
 * The Trash screen's view-model (CAD-2018). Pure, so the rules that decide
 * what an operator may send and what the screen may claim are testable
 * without a browser or a board.
 *
 * The board is authoritative for paths, custody and conflicts. These
 * helpers only keep the screen from claiming something it was not told, and
 * from sending an obviously unusable destination.
 */

/** A local pre-flight check, not the path grammar: the board's refusal is
 *  still the one the operator sees. Empty and slash-edged values are the
 *  only shapes worth saving a round trip for. */
export function destinationError(value: string): string | null {
  const path = value.trim();
  if (!path) return "Enter a destination path.";
  if (path.startsWith("/")) return "A wiki path cannot start with a slash.";
  if (path.endsWith("/")) return "A wiki path cannot end with a slash.";
  if (/[\\%\u0000-\u001f]/.test(path)) {
    return "A wiki path cannot contain a backslash, a percent sign or a control character.";
  }
  return null;
}

/**
 * Why a landing cannot be restored, or null when the board says it can.
 *
 * Only codes this UI can explain honestly are mapped. The custody refusal a
 * large hosted restore must send and the older digest-checkpoint refusal
 * are never presented as permission: a checkpoint is a point-in-time count,
 * not custody. A landing with no provable origin is always blocked here,
 * whatever the row claims, because there is no path to send. Any other code
 * is shown as the board's code, with no invented meaning.
 */
export function restoreBlock(
  landing: Pick<TrashLanding, "restorable" | "reason" | "from">,
): { text: string; code: string | null } | null {
  if (landing.restorable && landing.from !== null && landing.reason === null) return null;
  if (landing.from === null && (landing.reason === null || landing.reason === "origin_unverified")) {
    return {
      code: landing.reason ?? "origin_unverified",
      text: "The board cannot prove this landing’s exact original root, so it cannot say where a restore would put it. This screen does not guess a path and cannot restore it.",
    };
  }
  switch (landing.reason) {
    case "custody_unverified":
      return {
        code: "custody_unverified",
        text: "The board cannot prove custody of this landing’s stored bytes, so it refuses to restore them. A digest checkpoint from an older board is not custody, and this screen does not offer one as a workaround.",
      };
    case "verification_required":
      return {
        code: "verification_required",
        text: "This landing is larger than one verification page. A digest checkpoint proves the files were counted once, not that their bytes are still there, so this screen does not treat one as permission to restore.",
      };
    case "evidence_truncated":
      return {
        code: "evidence_truncated",
        text: "The board stopped searching older changes before it could prove this landing’s original path. It did not guess a restore source.",
      };
    case "origin_unverified":
      return {
        code: "origin_unverified",
        text: "The board cannot prove this landing’s exact original root, so it cannot say where a restore would put it.",
      };
    case "unreadable":
      return {
        code: "unreadable",
        text: "The board cannot read this landing’s stored rows, so it will not restore it.",
      };
    case "unsafe_tree":
      return {
        code: "unsafe_tree",
        text: "This landing holds a symlink or another entry the board will not move.",
      };
    case "unsafe_origin":
      return {
        code: "unsafe_origin",
        text: "This landing’s original root is outside the fixed wiki roots, so the board will not restore it.",
      };
    case "ambiguous_landing":
      return {
        code: "ambiguous_landing",
        text: "More than one stored landing could be this delete, so the board will not pick one.",
      };
    case "empty_landing":
      return {
        code: "empty_landing",
        text: "This landing has no entries left to restore.",
      };
    default:
      return {
        code: landing.reason,
        text: "The board marked this landing as not restorable.",
      };
  }
}

/** A restore is confirmed only by a receipt for the exact requested move. */
export function confirmedRestoreDestination(value: unknown, from: string, to: string): string | null {
  if (!value || typeof value !== "object") return null;
  const receipt = value as Record<string, unknown>;
  if (receipt.from !== from) return null;
  if (receipt.restored === true && receipt.path === to) return to;
  if (receipt.moved === true && receipt.to === to) return to;
  return null;
}

/** Native blob receipts may name the pointer with its `.blob` suffix. */
export function confirmedDeleteLanding(value: unknown, path: string): string | null {
  if (!value || typeof value !== "object") return null;
  const receipt = value as Record<string, unknown>;
  if (receipt.path !== path || typeof receipt.trash !== "string" || !safeTrashFrom(receipt.trash)) return null;
  const original = receipt.trash.split("/").slice(2).join("/");
  return original === path || original === `${path}.blob` ? receipt.trash : null;
}

/** Append one listing page in the board's order, never re-sorting it. A
 *  landing the cursor hands back twice keeps its first position. */
export function mergeLandings(current: TrashLanding[], page: TrashLanding[]): TrashLanding[] {
  const seen = new Set(current.map((landing) => landing.landing_id));
  const next = [...current];
  for (const landing of page) {
    if (seen.has(landing.landing_id)) continue;
    seen.add(landing.landing_id);
    next.push(landing);
  }
  return next;
}

/** Append one items page without hiding different kinds at the same path. */
export function mergeEntries(current: TrashEntry[], page: TrashEntry[]): TrashEntry[] {
  const seen = new Set(current.map((entry) => `${entry.kind}:${entry.path}`));
  const next = [...current];
  for (const entry of page) {
    const key = `${entry.kind}:${entry.path}`;
    if (seen.has(key)) continue;
    seen.add(key);
    next.push(entry);
  }
  return next;
}

/** The plain word for a landing's kind, for `t()` and composed sentences. */
export function kindWord(kind: WikiKind | null): string {
  return kind === "dir" ? "folder" : kind === "page" ? "page" : kind === "file" ? "file" : "item";
}
