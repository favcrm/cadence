/**
 * CAD-2471: a frame mounted in the canvas is read-only. The board's mount
 * route has no read-only request a tenant would accept, so the host narrows
 * the mount itself: the frame is told, and may call, only these read
 * operations, and the screen's tools never run. The action token stays with
 * the host and never reaches the frame, so an operation outside this list
 * has no path to the server.
 *
 * An allowlist, never a denylist: an operation added later is closed until it
 * is listed here. The list is the daemon's own read set for a member mount
 * (`ScreenOperation::is_screen_read`), so a screen that already runs for a
 * read-only member sees the operations it expects.
 */
import type { MountReceipt } from "./screenLifecycle";

export const READ_ONLY_OPERATIONS: readonly string[] = [
  "records.list", "records.show", "catalog.status",
  "segments.list", "segments.show", "exclusions.list", "exclusions.show", "suppressions.list",
  "audience.preview",
  "content.list", "content.show", "content.render",
  "proposals.list", "proposals.show", "proposals.render",
  "sends.list", "sends.show", "smtp.show", "origin.show",
  "screen.route.patch", "screen.preview.show", "screen.preview.clear",
  "whatsapp.status", "whatsapp.conversations.list", "whatsapp.conversations.show", "whatsapp.messages",
  "whatsapp.reply.status", "whatsapp.templates.list", "whatsapp.template.show", "whatsapp.template.status",
  "whatsapp.broadcasts.list", "whatsapp.broadcast.show", "whatsapp.broadcast.estimate", "whatsapp.broadcast.results",
  "whatsapp.draft.show", "campaign-work.show", "calendar.query", "records.aggregate", "works.query", "works.get",
];

/** The receipt a read-only mount is held to: operations narrowed to reads. */
export function readOnlyReceipt(receipt: MountReceipt): MountReceipt {
  if (receipt.operations === undefined) return receipt;
  return { ...receipt, operations: receipt.operations.filter((op) => READ_ONLY_OPERATIONS.includes(op)) };
}
