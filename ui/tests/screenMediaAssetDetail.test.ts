/**
 * CAD-1962 — `media.read` `{kind:"asset"}` detail must survive the frame
 * parser. The daemon admits the member, but a frame message is validated
 * here first: refusing it closes the mount before any RPC runs, so the
 * read would exist and never be reachable.
 *
 * Pure parser check: no renderer, no network, no authority proof.
 */
import { isMediaVerb, mediaImportArgs, mediaReadArgs, MEDIA_VERBS } from "../src/features/workspace-apps/screen/screenProtocol";
import { parseMount } from "../src/features/workspace-apps/screen/screenLifecycle";

const ID = `med-${"a".repeat(32)}`;
const OTHER = `med-${"b".repeat(32)}`;

function equal(actual: unknown, expected: unknown, why: string) {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(`${why}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}
function refused(actual: unknown, why: string) {
  if (actual !== null) throw new Error(`${why}: expected null, got ${JSON.stringify(actual)}`);
}

// Admitted: exactly the id and the kind.
equal(mediaReadArgs({ kind: "asset", asset_id: ID }), { kind: "asset", asset_id: ID }, "canonical asset detail");

// Refused: any extra field (a forged selector must never be stripped).
refused(mediaReadArgs({ kind: "asset", asset_id: ID, limit: 5 }), "extra limit");
refused(mediaReadArgs({ kind: "asset", asset_id: ID, install_id: "other" }), "forged install scope");
refused(mediaReadArgs({ kind: "asset", asset_id: ID, cursor: "x" }), "extra cursor");

// Refused: missing or non-canonical id.
refused(mediaReadArgs({ kind: "asset" }), "missing id");
refused(mediaReadArgs({ kind: "asset", asset_id: "med-zzzz" }), "malformed id");
refused(mediaReadArgs({ kind: "asset", asset_id: OTHER.toUpperCase() }), "uppercase id");
refused(mediaReadArgs({ kind: "asset", asset_id: 7 }), "non-string id");

// Unchanged neighbours.
equal(mediaReadArgs({ kind: "catalog" }), { kind: "catalog" }, "catalog still admitted");
equal(mediaReadArgs({ kind: "catalog", limit: 20 }), { kind: "catalog", limit: 20 }, "catalog limit still admitted");
refused(mediaReadArgs({ kind: "catalog", limit: 21 }), "catalog limit bound unchanged");
equal(
  mediaReadArgs({ kind: "preview", asset_id: ID, expected_revision: 2 }),
  { kind: "preview", asset_id: ID, expected_revision: 2 },
  "preview still admitted",
);
refused(mediaReadArgs({ kind: "media.generate" }), "generate stays unadmitted");

// CAD-1962 import: the mount must admit the permission the daemon mints,
// or the receipt is rejected and the screen never mounts.
equal(isMediaVerb("media.import"), true, "media.import is a media verb");
equal(MEDIA_VERBS.length, 4, "all four declared media permissions are admitted");
equal(mediaImportArgs({ receipt_id: "receipt-1", request_id: "req-1" }), { receipt_id: "receipt-1", request_id: "req-1" }, "canonical import args");
refused(mediaImportArgs({ receipt_id: "receipt-1", request_id: "req-1", asset_id: ID }), "import extra field");
refused(mediaImportArgs({ receipt_id: "receipt-1" }), "import missing request id");
refused(mediaImportArgs({ receipt_id: "", request_id: "req-1" }), "import empty receipt");
refused(mediaImportArgs({ receipt_id: "receipt 1", request_id: "req-1" }), "import receipt grammar");
refused(mediaImportArgs({ receipt_id: "receipt-1", request_id: "req/1" }), "import request grammar");

const mountReceipt = {
  mount: `/api/app-screen/${"a".repeat(64)}`,
  bridge_nonce: "b".repeat(64),
  generation: 1,
  tag: "main",
  action_token: "c".repeat(64),
  media: {
    contract: "screen-media.v1",
    may: ["media.read", "media.publish", "media.import", "media.chat.attach"],
  },
};
equal(parseMount(mountReceipt, "main") !== null, true, "a mount declaring media.import is admitted");
const unknownVerb = { ...mountReceipt, media: { contract: "screen-media.v1", may: ["media.generate"] } };
equal(parseMount(unknownVerb, "main"), null, "an unknown media verb still refuses the mount");
