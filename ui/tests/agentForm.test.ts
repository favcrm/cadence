import { formStatus, threadItems, threadRuns, type PendingMessage, type ThreadEntry, type ThreadState } from "../src/features/home/thread";

function check(condition: boolean, message: string): void {
  if (!condition) throw new Error(`agentForm: ${message}`);
}

const agent = (seq: number, kind: string, text = "", message = "m1", payload: Record<string, unknown> | null = null): ThreadEntry => ({
  seq,
  role: "agent",
  kind,
  text,
  message,
  payload,
});
const operator = (seq: number, text: string, refs?: { kind: string; id: string }[]): ThreadEntry => ({
  seq,
  role: "operator",
  kind: "message",
  text,
  message: `op${seq}`,
  payload: refs ? { refs } : null,
  created: "2026-11-13T10:42:00Z",
});
const pending = (message: string, state: PendingMessage["state"], refs?: { kind: string; id: string }[]): PendingMessage =>
  ({ message, text: "answer", state, at: 0, refs }) as PendingMessage;

const form = agent(3, "surface", "Weekly blocked summary\n- Day: Monday, Friday (choose one)", "m1", {
  a2ui: { version: "1.0", createSurface: { surfaceId: "s1", components: [{ id: "root", component: "Card", child: "t" }] } },
  digest: "d",
});
const base = [operator(1, "Send me a weekly summary"), agent(2, "tool_call", "master ask-form"), form, agent(4, "tool_result", "ok")];
const own = [{ kind: "surface", id: "3" }];

// Open until something answers or overtakes it.
check(formStatus(3, base, []).state === "open", "a fresh form is open");

// Its own answer marks it sent, with the answer's time; it does not close it.
const sent = formStatus(3, [...base, operator(5, "Day: Monday", own)], []);
check(sent.state === "sent" && sent.at === "2026-11-13T10:42:00Z", "the citing operator message is the form's answer");

// Any other later operator message closes it, so a late answer cannot be sent.
check(formStatus(3, [...base, operator(5, "just Fridays")], []).state === "closed", "a typed reply closes the form");
check(formStatus(3, base, [pending("p1", "sending")]).state === "closed", "a typed reply still in flight closes it too");
// An earlier operator message does not.
check(formStatus(3, [operator(0, "earlier"), ...base], []).state === "open", "messages before the form do not close it");
// A different form's answer closes this one.
check(formStatus(3, [...base, operator(5, "x", [{ kind: "surface", id: "9" }])], []).state === "closed", "another form's answer is not this one's");

// Its own pending answer: sending, then failed with the row to retry.
check(formStatus(3, base, [pending("p1", "sending", own)]).state === "sending", "its answer in flight reads sending");
const failed = formStatus(3, base, [pending("p1", "failed", own)]);
check(failed.state === "failed" && failed.pending.message === "p1", "a failed answer keeps its message id for Retry");

// The form waits for its tool run to end and stays inside the turn.
const state = { entries: [...base, agent(5, "turn_result", "I put a form above.")], pending: [] } as unknown as ThreadState;
const items = threadItems(state);
const types = items.map((i) => i.type);
check(types.join(",") === "operator,tools,form,answer", `the form follows its tool run, got ${types.join(",")}`);
const runs = threadRuns(items);
check(runs.length === 2 && runs[1].turn === true && runs[1].items.some((i) => i.type === "form"), "the form is a card of the master's turn");

console.log("agentForm: ok");
