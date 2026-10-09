import { addFiles, attachItems, removeAttach, retryAttach } from "../src/features/app-shell/chat/composerStore";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

// A refused kind lands as a failed, permanent row with a visible reason;
// Retry never re-sends it (no upload is attempted, the row is unchanged).
const key = "attach-store-test";
addFiles(key, [new File(["%PDF-1.4"], "brief.pdf")]);
const [pdf] = attachItems(key);
equal(pdf.status, "failed", "a PDF is refused at selection");
equal(pdf.permanent, true, "a pre-flight refusal is permanent");
equal(String(pdf.error).includes("PDF and image processing is not available yet"), true, "the reason is specific");
retryAttach(key, pdf.key);
equal(attachItems(key)[0].status, "failed", "Retry on a permanent refusal changes nothing");

// The sixth file is refused at selection and stays refused on Retry.
for (let i = 0; i < 4; i++) addFiles(key, [new File(["x"], `bad${i}.exe`)]);
equal(attachItems(key).length, 5, "five rows queued");
addFiles(key, [new File(["x"], "sixth.txt")]);
const sixth = attachItems(key)[5];
equal(sixth.status, "failed", "the sixth file is refused");
equal(sixth.permanent, true, "the count refusal is permanent");
retryAttach(key, sixth.key);
equal(attachItems(key)[5].status, "failed", "Retry on the sixth file is a no-op, never a sixth ready row");

for (const row of attachItems(key)) removeAttach(key, row.key);
console.log("chatAttachStore.test.ts: all assertions passed");
