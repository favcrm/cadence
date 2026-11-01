/**
 * CAD-1962 — `media.read` `{kind:"asset"}` detail must survive the frame
 * parser. The daemon admits the member, but a frame message is validated
 * here first: refusing it closes the mount before any RPC runs, so the
 * read would exist and never be reachable.
 *
 * Pure parser check: no renderer, no network, no authority proof.
 */
import { mediaReadArgs } from "../src/features/workspace-apps/screen/screenProtocol";

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
