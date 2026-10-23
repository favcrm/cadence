import { threadItems, type ThreadEntry, type ThreadState } from "../src/features/home/thread";

function check(condition: boolean, message: string): void {
  if (!condition) throw new Error(`hireProposal: ${message}`);
}

const entry = (seq: number, kind: string, text: string, message = "m1", payload?: Record<string, unknown>): ThreadEntry => ({
  seq,
  role: "agent",
  kind,
  text,
  message,
  payload: payload ?? null,
});

const state = {
  entries: [
    entry(1, "assistant_text", "An Accountant fits: it reads your invoices and drafts reminders."),
    entry(2, "hire_proposal", "Proposed hire: Accountant", "m1", { role: "accountant", name: null, job: "j", digest: "d" }),
    entry(3, "turn_result", "Done."),
  ],
  pending: [],
} as unknown as ThreadState;

const items = threadItems(state);
const hire = items.find((i) => i.type === "hire");
check(hire !== undefined && hire.type === "hire", "a hire_proposal entry becomes a hire card item");
check(hire?.type === "hire" && hire.reason === "An Accountant fits: it reads your invoices and drafts reminders.", "its reasoning is the turn's own commentary");

// The reasoning moves into the card: the commentary is not shown a second time.
check(!items.some((i) => i.type === "commentary"), "the commentary line is folded into the card");

// Without commentary right before it, the card has no reasoning line; another
// turn's text never stands in.
const bare = {
  entries: [
    entry(1, "assistant_text", "unrelated", "m9"),
    entry(2, "hire_proposal", "Proposed hire: QA tester", "m2", { role: "qa", job: "j", digest: "d" }),
  ],
  pending: [],
} as unknown as ThreadState;
const bareHire = threadItems(bare).find((i) => i.type === "hire");
check(bareHire?.type === "hire" && bareHire.reason === null, "no same-turn commentary, no reasoning");
check(threadItems(bare).some((i) => i.type === "commentary"), "another turn's commentary stays");
