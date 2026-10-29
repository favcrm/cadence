/** CAD-1941: Threads context uses recorded tasks, tickets and artifact cards. */
import { composerDraftState } from "../src/features/app-shell/chat/composerStore";
import { parseAskRef } from "../src/features/canvas/askRefs";
import type { ThreadEntry } from "../src/features/home/thread";
import { askAboutArtifact } from "../src/features/threads/WorkContext";
import {
  assigneeIssues,
  assigneeTasks,
  canvasFollow,
  conversationOutputs,
} from "../src/features/threads/threadsContext";
import type { Agent, AgentTask } from "../src/lib/types";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

function task(id: string, state: string, title: string): AgentTask {
  return { task: id, task_state: state, title, issue: "", job: "job-1", job_state: "running" };
}
function agent(tasks: AgentTask[], on: string[] = []): Agent {
  return { alias: "nova", tasks, on } as Agent;
}

const tasks = assigneeTasks(agent([
  task("research", "done", "Read the tender"),
  task("draft", "running", "Draft the proposal"),
]));
equal(tasks.map((item) => [item.id, item.state, item.title]), [
  ["research", "done", "Read the tender"],
  ["draft", "running", "Draft the proposal"],
], "each task keeps its own state");

const bare = agent([]);
equal(assigneeTasks(bare), [], "no recorded tasks");
equal(assigneeIssues(bare), [], "a title is not a ticket");
equal(assigneeIssues(agent([], ["CAD-12"])), ["CAD-12"], "only the recorded issue id is a ticket");

const card = (seq: number, payload: Record<string, unknown> | null): ThreadEntry => ({
  seq, role: "agent", kind: "artifact", text: "", payload,
});
const outputs = conversationOutputs([
  card(1, { id: "art-mock", version: 2, kind: "html", title: "Checkout", state: "draft" }),
  card(2, { id: "not-an-artifact", version: 1, kind: "html", title: "Bad" }),
  { seq: 3, role: "agent", kind: "assistant_text", text: "CAD-99 looks done", payload: null },
]);
equal(outputs.map((item) => [item.ref?.id ?? null, item.ref?.kind ?? null, item.ref?.version ?? null, item.state]), [
  ["art-mock", "html", 2, "draft"],
  [null, null, null, null],
], "a valid card keeps kind, version and state; an invalid card stays unavailable");
equal(conversationOutputs([]), [], "no cards means no outputs");

const key = "threads:nova";
askAboutArtifact(key, { id: "art-mock", version: 2, kind: "html", title: "Checkout", bytes: 0 });
const draft = composerDraftState(key);
assert(draft.text === "", "asking does not write or send a message");
assert(draft.refs.length === 1, "the draft keeps one citation");
equal(parseAskRef(draft.refs[0]), { artifact: "art-mock", version: 2 }, "the citation names that artifact version");

equal(canvasFollow({ conversation: "a", composerKey: "threads:a" }, "a", "b", "threads:b"), "reset", "a canvas owned by the previous conversation resets");
equal(canvasFollow({ conversation: "other", composerKey: null }, "a", "b", "threads:b"), "keep", "a canvas owned elsewhere stays");
equal(canvasFollow({ conversation: "a", composerKey: "threads:a" }, "a", "a", "threads:a:reply"), "rebind", "the same conversation follows the composer draft");
equal(canvasFollow({ conversation: "a", composerKey: "threads:a" }, null, "a", "threads:a"), "keep", "the first selection does not reset");

console.log("threadsWork: passed");
