/**
 * A native board answers an occupied create-only destination — and a stale
 * update — with HTTP 200 `{conflict:"if_rev"}` after storing nothing. The
 * multipart upload adapter must surface that refusal, not report the bytes as
 * saved. The board is stubbed at `fetch`; the adapter and its error mapping
 * are the real modules.
 */
import { WikiError } from "../src/features/wiki/api";
import { uploadMultipart } from "../src/features/wiki/transfer";
import { beginPrivateOperation } from "../src/lib/privateSession";

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

let uploads = 0;
let reply: unknown = null;
globalThis.fetch = async (input) => {
  const url = new URL(String(input), "http://localhost");
  if (url.pathname !== "/api/wiki/upload") throw new Error(`unexpected ${url.pathname}`);
  uploads += 1;
  return new Response(JSON.stringify(reply), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
};

async function upload(): Promise<unknown> {
  const operation = beginPrivateOperation();
  try {
    return await uploadMultipart(
      "global",
      new File(["replacement bytes"], "brief.md"),
      false,
      () => undefined,
      operation,
    );
  } finally {
    operation.dispose();
  }
}

async function main(): Promise<void> {
  // A refused write: the board stored nothing.
  reply = { conflict: "if_rev", current_rev: "newer-rev", path: "global/brief.md" };
  try {
    await upload();
    throw new Error("a refused upload was reported as saved");
  } catch (error) {
    assert(error instanceof WikiError, "the refusal is a WikiError");
    assert(error.status === 409, "the refusal keeps its conflict status");
  }
  assert(uploads === 1, "the conflict was reported for the upload that was attempted");

  // The same adapter still reports a real save.
  reply = { kind: "text", rev: "saved-rev", path: "global/brief.md" };
  const saved = (await upload()) as { outcome: string; index: string };
  assert(saved.outcome === "applied" && saved.index === "covered", "a stored page is still reported as saved");

  console.log("wiki upload conflict checks passed");
}
void main();
