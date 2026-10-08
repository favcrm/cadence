import {
  ATTACH_EXTS,
  ATTACH_MAX_BYTES,
  ATTACH_MAX_FILES,
  attachFits,
  attachMeta,
  isPermanentUploadFailure,
  userUploadError,
  attachRefusal,
  unresolved,
  unresolvedHint,
  type AttachItem,
} from "../src/features/app-shell/chat/attach";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

// ---- the v1 allowlist and cap (client pre-check; the daemon re-checks) ---

equal(ATTACH_EXTS, ["txt", "md", "csv"], "the interim processable allowlist");
equal(attachRefusal("brief.pdf", 100)?.includes("not available yet"), true, "PDF is honestly unavailable");
equal(attachRefusal("reference.png", 100)?.includes("not available yet"), true, "image is honestly unavailable");
equal(ATTACH_MAX_BYTES, 10 * 1024 * 1024, "10 MiB cap");

equal(attachRefusal("notes.txt", 100), null, "a small text file goes");
equal(attachRefusal("brief.CSV", 100), null, "extensions are case-insensitive");
equal(attachRefusal("a.exe", 100) !== null, true, "an off-list extension refuses");
equal(attachRefusal("noext", 100) !== null, true, "no extension refuses");
equal(attachRefusal("a.txt", 0) !== null, true, "an empty file refuses");
equal(attachRefusal("a.txt", ATTACH_MAX_BYTES), null, "exactly at the cap goes");
equal(attachRefusal("a.txt", ATTACH_MAX_BYTES + 1) !== null, true, "one byte over refuses");

// ---- the count cap --------------------------------------------------------

equal(attachFits(0, ATTACH_MAX_FILES), null, "a full batch of five fits");
equal(attachFits(ATTACH_MAX_FILES - 1, 1), null, "the fifth file fits");
equal(attachFits(ATTACH_MAX_FILES, 1) !== null, true, "the sixth refuses");

// ---- only ready rows attach ----------------------------------------------

const item = (key: string, status: AttachItem["status"], id?: string): AttachItem =>
  ({ key, name: `${key}.txt`, size: 1, status, file: null as unknown as File, id, generation: 0 });

// ---- the send gate: unresolved rows hold the send ---------------------

equal(unresolved([item("a", "ready", "chf-1")]).length, 0, "a ready queue never holds a send");
equal(unresolved([item("a", "uploading"), item("b", "ready", "chf-1")]).length, 1, "uploading holds");
equal(unresolved([item("a", "failed")]).length, 1, "a failed row holds until retried or removed");
equal(unresolvedHint([item("a", "ready", "chf-1")]), null, "no hold reason when all ready");
equal(
  (unresolvedHint([item("a", "uploading")]) ?? "").includes("still uploading"),
  true,
  "the hold reason names the pending upload",
);
equal(
  (unresolvedHint([item("a", "failed")]) ?? "").includes("failed"),
  true,
  "the hold reason names the failed file",
);

// ---- visible, specific refusal reasons (CAD-1168 fix 1) ---------------------

equal(
  userUploadError("chat_file_upload refused 'fake.txt': a '.txt' file must sniff as its declared kind — the bytes did not"),
  "A '.txt' file must sniff as its declared kind — the bytes did not",
  "the RPC name and file prefix never reach the operator",
);
equal(
  userUploadError("chat_file_upload: '.exe' is not an attachable type — the interim allowlist is txt, md and csv"),
  "'.exe' is not an attachable type",
  "the allowlist tail is dropped; the reason stays",
);
equal(userUploadError("upload failed — connection lost"), "Upload failed — connection lost", "plain messages stay");
equal(isPermanentUploadFailure(400), true, "a server refusal is permanent");
equal(isPermanentUploadFailure(413), true, "an oversize refusal is permanent");
equal(isPermanentUploadFailure(0), false, "a lost connection is retryable");
equal(isPermanentUploadFailure(503), false, "a 5xx is retryable");
equal(isPermanentUploadFailure(429), false, "a rate limit is retryable");
equal(isPermanentUploadFailure(undefined), false, "an abort is retryable");
equal(attachMeta("customers.csv", 180 * 1024), "180 KB · CSV", "size and kind like the mock's file rows");
equal(
  (unresolvedHint([{ ...item("a", "failed"), error: "PDF and image processing is not available yet — .pdf cannot be attached" }]) ?? "").includes(".pdf cannot be attached"),
  true,
  "the hold line carries the specific reason, not a generic count",
);

console.log("chatAttach.test.ts: all assertions passed");
