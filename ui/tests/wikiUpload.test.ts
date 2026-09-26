import {
  capLabel,
  counts,
  MAX_UPLOAD_BYTES,
  pending,
  queued,
  serverErrorText,
  sizeError,
  startUpload,
  uploadDone,
  uploadFailed,
  uploadProgress,
  type UploadItem,
} from "../src/features/wiki/upload";
import { WikiError } from "../src/features/wiki/api";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

const MB = 1024 * 1024;

// ---- the client pre-check (the server's cap, CAD-580) --------------------

equal(MAX_UPLOAD_BYTES, 100 * MB, "the cap is 100 MB");
equal(capLabel(), "100 MB", "the cap's label");
equal(sizeError(1024), null, "a small file goes");
equal(sizeError(100 * MB), null, "exactly at the cap goes");
equal(sizeError(100 * MB + 1), "over 100 MB — rejected", "one byte over is rejected");
equal(sizeError(212 * MB), "over 100 MB — rejected", "and so is 212 MB");
equal(sizeError(50 * MB, 20 * MB), "over 20 MB — rejected", "a configured cap moves the line");

// ---- the queue -----------------------------------------------------------

const queuedItems = queued(
  [
    { name: "hero-dark.png", size: 640 * 1024 },
    { name: "kit-notes.md", size: 8 * 1024 },
    { name: "full-recording.mp4", size: 212 * MB },
  ],
  0,
);
equal(
  queuedItems.map((i) => `${i.id}:${i.name}:${i.state}`),
  ["u0:hero-dark.png:queued", "u1:kit-notes.md:queued", "u2:full-recording.mp4:error"],
  "an oversize file starts failed and the rest queue",
);
equal(queuedItems[2].error, "over 100 MB — rejected", "the oversize row carries the reason");
equal(pending(queuedItems), ["u0", "u1"], "only the files under the cap are pending");
equal(counts(queuedItems), { done: 0, failed: 1, total: 3 }, "the tally counts the pre-check's failure");

// ids continue across selections, so a second drop never reuses one.
equal(queued([{ name: "b.png", size: 1 }], 3)[0].id, "u3", "ids continue from the counter");

// ---- the per-file state machine -----------------------------------------

let items: UploadItem[] = queuedItems;
items = startUpload(items, "u0");
equal(items[0].state, "uploading", "the first file starts");
items = uploadProgress(items, "u0", 72.4);
equal(items[0].progress, 72, "progress is rounded and kept per file");
equal(items[1].progress, 0, "the next file is untouched");
items = uploadProgress(items, "u0", 140);
equal(items[0].progress, 100, "progress is clamped");
items = uploadDone(items, "u0");
equal([items[0].state, items[0].progress, items[0].error], ["done", 100, undefined], "done clears the error");

// The server's own refusal lands on that file alone.
items = uploadFailed(items, "u1", "file is over the 100 MB cap");
equal(items[1].state, "error", "the server's refusal marks the row");
equal(items[1].error, "file is over the 100 MB cap", "with the server's message");
equal(items[0].state, "done", "the finished file keeps its state");
equal(counts(items), { done: 1, failed: 2, total: 3 }, "the tally after one done, one refused, one pre-rejected");
equal(pending(items), [], "nothing is left pending");

equal(serverErrorText(new WikiError("over 100 MB — rejected", 413)), "over 100 MB — rejected", "an API error reads its message");
equal(serverErrorText(new Error("connection lost")), "connection lost", "a plain error reads its message");
equal(serverErrorText("just a string"), "just a string", "anything else stringifies");

console.log("wiki upload checks passed");
