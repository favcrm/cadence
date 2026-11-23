import { addFiles, carryEarlyDraft, composerDraftState, setComposerDraft, attachItems, removeAttach, retryAttach } from "../src/features/app-shell/chat/composerStore";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

// An empty file lands as a permanent refusal; retry never re-sends it.
const key = "attach-store-test";
addFiles(key, [new File([], "empty.txt")]);
const [empty] = attachItems(key);
equal(empty.status, "failed", "an empty file is refused at selection");
equal(empty.permanent, true, "a pre-flight refusal is permanent");
retryAttach(key, empty.key);
equal(attachItems(key)[0].status, "failed", "Retry on a permanent refusal changes nothing");

// The sixth file is refused at selection and stays refused on Retry.
for (let i = 0; i < 4; i++) addFiles(key, [new File([], `empty${i}.txt`)]);
equal(attachItems(key).length, 5, "five rows queued");
addFiles(key, [new File(["x"], "sixth.txt")]);
const sixth = attachItems(key)[5];
equal(sixth.status, "failed", "the sixth file is refused");
equal(sixth.permanent, true, "the count refusal is permanent");
retryAttach(key, sixth.key);
equal(attachItems(key)[5].status, "failed", "Retry on the sixth file is a no-op, never a sixth ready row");

for (const row of attachItems(key)) removeAttach(key, row.key);
// CAD-1279: text typed before the conversation resolved (key `app|i|`)
// is lost when the pane remounts under `app|i|c1`; carrying it moves it
// once, never over another draft and never into a different install.
setComposerDraft("app|i|", "typed early");
equal(composerDraftState("app|i|c1").text, "", "before the carry the resolved key is empty (the lost draft)");
carryEarlyDraft("app|i|", "app|i|c1");
equal(composerDraftState("app|i|c1").text, "typed early", "the early draft survives the key change");
equal(composerDraftState("app|i|").text, "", "the early slot is emptied, not duplicated");
setComposerDraft("app|i|", "second");
carryEarlyDraft("app|i|", "app|i|c1");
equal(composerDraftState("app|i|c1").text, "typed early", "an existing draft is never overwritten");
equal(composerDraftState("app|i|").text, "second", "the refused carry leaves the early text in place");
setComposerDraft("app|i|", "");
setComposerDraft("app|i|c1", "");

console.log("chatAttachStore.test.ts: all assertions passed");
