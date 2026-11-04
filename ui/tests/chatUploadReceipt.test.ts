/**
 * CAD-2152: the board UI's decode of `POST /api/chat/upload`. The hosted
 * fixture is exactly the field set agenticos-v2's
 * `tenant-board/assistant/upload-routes.ts` answers; the native fixture is the
 * daemon's `ChatFile::to_json`. Each shape is accepted strictly and neither
 * borrows the other's fields.
 */
import { decodeChatFileRow } from "../src/lib/api";

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

const hosted = {
  id: `chf-${"a1".repeat(16)}`,
  name: "brief.pdf",
  size: 625,
  mime: "application/pdf",
  sha256: "b".repeat(64),
  conversation: "conv-1",
  created: 1_762_000_000_000,
};
const native = {
  id: `chf-${"c2".repeat(16)}`,
  sha256: "d".repeat(64),
  size: 10,
  mime: "text/plain",
  name: "a.txt",
  scope: "home",
  context_id: "",
  uploader: "operator",
  user_id: `acu-${"e3".repeat(16)}`,
  thread_id: "t-1",
  location_version: 1,
  created: 1,
};

const accepted = decodeChatFileRow(hosted);
assert(accepted.id === hosted.id && accepted.name === "brief.pdf" && accepted.mime === "application/pdf", "hosted receipt accepted");
assert(decodeChatFileRow(native).id === native.id, "native receipt still accepted");

function refuses(label: string, value: unknown): void {
  try {
    decodeChatFileRow(value);
  } catch (error) {
    assert(error instanceof Error && /invalid attachment receipt/.test(error.message), `${label}: wrong error`);
    return;
  }
  throw new Error(`${label}: accepted`);
}

const without = (row: Record<string, unknown>, key: string) => {
  const { [key]: _drop, ...rest } = row;
  return rest;
};
for (const key of Object.keys(hosted)) refuses(`hosted missing ${key}`, without(hosted, key));
refuses("hosted bad id", { ...hosted, id: "chf-xyz" });
refuses("hosted native-style id", { ...hosted, id: `acu-${"a1".repeat(16)}` });
refuses("hosted bad sha", { ...hosted, sha256: "short" });
refuses("hosted zero size", { ...hosted, size: 0 });
refuses("hosted fractional size", { ...hosted, size: 1.5 });
refuses("hosted empty mime", { ...hosted, mime: "" });
refuses("hosted blank name", { ...hosted, name: "  " });
refuses("hosted empty conversation", { ...hosted, conversation: "" });
refuses("hosted non-numeric created", { ...hosted, created: "now" });
refuses("hosted mixed with native identity", { ...hosted, user_id: native.user_id, thread_id: "t" });
for (const key of Object.keys(native)) refuses(`native missing ${key}`, without(native, key));
refuses("native bad user", { ...native, user_id: "u1" });
refuses("not an object", "x");
refuses("null", null);
refuses("array", [hosted]);
