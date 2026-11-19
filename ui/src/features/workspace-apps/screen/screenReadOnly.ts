/**
 * CAD-2471: a frame mounted in the canvas is read-only. The board's mount
 * route has no read-only request a tenant would accept, so the host narrows
 * the mount itself: the frame is told, and may call, only these read
 * operations, and the screen's tools never run. The action token stays with
 * the host and never reaches the frame, so an operation outside this list
 * has no path to the server.
 *
 * An allowlist, never a denylist: an operation added later is closed until it
 * is listed here. The list follows the daemon's own read set for a member
 * mount (`ScreenOperation::is_screen_read`), narrowed to what a record view
 * needs.
 */
import type { MountReceipt } from "./screenLifecycle";

export const READ_ONLY_OPERATIONS: readonly string[] = [
  "records.list", "records.show", "records.aggregate",
  "segments.list", "segments.show", "exclusions.list", "exclusions.show", "suppressions.list",
  "content.list", "content.show", "content.render",
  "proposals.list", "proposals.show", "proposals.render",
  "sends.list", "sends.show", "campaign-work.show",
  "whatsapp.status", "whatsapp.conversations.list", "whatsapp.conversations.show", "whatsapp.messages",
  "works.query", "works.get", "calendar.query",
  "screen.route.patch",
];

/** The receipt a read-only mount is held to: operations narrowed to reads. */
export function readOnlyReceipt(receipt: MountReceipt): MountReceipt {
  if (receipt.operations === undefined) return receipt;
  return { ...receipt, operations: receipt.operations.filter((op) => READ_ONLY_OPERATIONS.includes(op)) };
}
