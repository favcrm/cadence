import { formStatus, threadItems, threadRuns, type PendingMessage, type ThreadEntry, type ThreadState } from "../src/features/home/thread";
import { ACTION_MARK, answeredData, failedCheck, parse, walk, type Comp } from "../src/features/home/formModel";

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
check(sent.state === "sent" && sent.answer === "Day: Monday", "a sent status carries its answer's text");

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

// CAD-2346: a sent form shows what was answered, read from the answer's
// A2UI action, never the surface's defaults — also after a reload.
const compiled = agent(6, "surface", "Review", "m1", {
  a2ui: {
    version: "1.0",
    createSurface: {
      surfaceId: "review",
      components: [
        { id: "root", component: "Card", child: "body" },
        { id: "body", component: "Column", children: ["field_name", "field_day", "field_mail", "field_ok", "submit"] },
        { id: "field_name", component: "TextField", label: "Your name", value: { "@path": "/name" } },
        { id: "field_day", component: "ChoicePicker", label: "Day", variant: "mutuallyExclusive", options: [{ label: "Monday", value: "mon" }, { label: "Friday", value: "fri" }], value: { "@path": "/day" } },
        { id: "field_mail", component: "TextField", label: "Email", value: { "@path": "/mail" } },
        { id: "field_ok", component: "CheckBox", label: "Remind me", value: { "@path": "/ok" } },
        { id: "submit", component: "Button", child: "submit_label", action: { event: { name: "answer", context: { name: { "@path": "/name" }, day: { "@path": "/day" }, mail: { "@path": "/mail" }, ok: { "@path": "/ok" } } } } },
        { id: "submit_label", component: "Text", text: "Send" },
      ],
      dataModel: { name: "", day: ["mon"], mail: "", ok: true },
    },
  },
  digest: "d",
});
const surface = parse(compiled)!;
const order = walk(surface.comps);
const action = { version: "1.0", action: { name: "answer", surfaceId: "review", sourceComponentId: "submit", timestamp: "2026-11-16T09:00:00Z", context: { name: "QA Tester", day: ["fri"], mail: "", ok: false } } };
const reloaded = answeredData(order, surface.data, `Form answer (review):\nYour name: QA Tester\nDay: Friday${ACTION_MARK}${JSON.stringify(action)}`);
check(reloaded.known, "the answer's action is read");
check(reloaded.data.name === "QA Tester", `the answered name shows, got ${JSON.stringify(reloaded.data.name)}`);
check(JSON.stringify(reloaded.data.day) === '["fri"]', `the answered choice shows, not the default Monday, got ${JSON.stringify(reloaded.data.day)}`);
check(reloaded.data.ok === false, "an answered unticked box shows unticked, not the default tick");
// An answer whose action cannot be read shows blank inputs, never the defaults.
for (const broken of ["Day: Friday", `x${ACTION_MARK}{not json`, `x${ACTION_MARK}${JSON.stringify({ action: { context: ["fri"] } })}`]) {
  const blank = answeredData(order, surface.data, broken);
  check(!blank.known, `an unreadable answer is not known: ${broken}`);
  check(blank.data.name === "" && JSON.stringify(blank.data.day) === "[]" && blank.data.ok === false, `an unreadable answer blanks every input: ${broken}`);
}
// A key the answer leaves out stays blank.
const partial = answeredData(order, surface.data, `x${ACTION_MARK}${JSON.stringify({ action: { context: { name: "Only name" } } })}`);
check(partial.known && partial.data.name === "Only name" && JSON.stringify(partial.data.day) === "[]", "a missing key stays blank");

// With two buttons, the one that sent the answer names the bindings.
const two = parse(agent(7, "surface", "Pick", "m1", {
  a2ui: {
    version: "1.0",
    createSurface: {
      surfaceId: "pick",
      components: [
        { id: "root", component: "Column", children: ["a", "b", "yes", "no"] },
        { id: "a", component: "TextField", label: "A", value: { "@path": "/a" } },
        { id: "b", component: "TextField", label: "B", value: { "@path": "/b" } },
        { id: "yes", component: "Button", child: "t", action: { event: { name: "yes", context: { v: { "@path": "/a" } } } } },
        { id: "no", component: "Button", child: "t", action: { event: { name: "no", context: { v: { "@path": "/b" } } } } },
      ],
      dataModel: { a: "", b: "" },
    },
  },
  digest: "d",
}))!;
const viaNo = answeredData(walk(two.comps), two.data, `x${ACTION_MARK}${JSON.stringify({ action: { sourceComponentId: "no", context: { v: "picked" } } })}`);
check(viaNo.data.b === "picked" && viaNo.data.a === "", "the sending button's bindings place the value");
for (const source of ["gone", "t"]) {
  const unread = answeredData(walk(two.comps), two.data, `x${ACTION_MARK}${JSON.stringify({ action: { sourceComponentId: source, context: { v: "picked" } } })}`);
  check(!unread.known && unread.data.a === "" && unread.data.b === "", `an answer naming button '${source}' without a context is not read`);
}
const unnamed = answeredData(walk(two.comps), two.data, `x${ACTION_MARK}${JSON.stringify({ action: { context: { v: "picked" } } })}`);
check(unnamed.known && unnamed.data.a === "picked", "an answer that names no button falls back to the first");

// CAD-2346: an optional blank email passes; only `required` refuses a blank.
const email = (required: boolean): Comp => ({
  id: "field_mail",
  component: "TextField",
  value: { "@path": "/mail" },
  "@checks": [
    ...(required ? [{ condition: { "@call": "required", args: { value: { "@path": "/mail" } } }, message: "Required." }] : []),
    { condition: { "@call": "email", args: { value: { "@path": "/mail" } } }, message: "Enter an email address." },
  ],
});
check(failedCheck(email(false), { mail: "" }) === null, "an optional blank email passes");
check(failedCheck(email(true), { mail: "" }) === "Required.", "a required blank email is refused by required");
check(failedCheck(email(false), { mail: "not-an-email" }) === "Enter an email address.", "a malformed email is still refused");
check(failedCheck(email(false), { mail: "qa@example.com" }) === null, "a valid email passes");

console.log("agentForm: ok");
