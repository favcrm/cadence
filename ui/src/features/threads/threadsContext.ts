/**
 * CAD-1941: the Threads context panel's view of work the board already
 * recorded. Tasks come from the assignee's roster row, tickets from that
 * row's `on` ids, and outputs from artifact cards already on the
 * conversation. Nothing here is parsed out of message text.
 */
import { agentOn } from "../../lib/agentOn";
import type { Agent, AgentTask } from "../../lib/types";
import type { ArtifactRef } from "../canvas/canvasStore";
import { artifactRef } from "../canvas/feed";
import type { ThreadEntry } from "../home/thread";
import type { ConversationRow } from "./threadsClient";

/** One task as the roster recorded it. `state` is that task's own state. */
export interface ContextTask {
  id: string;
  title: string;
  state: string;
}

/** One artifact card already on the conversation, or a card the thread
 *  stored that does not validate. A missing ref is not replaced. */
export interface ContextOutput {
  key: string;
  ref: ArtifactRef | null;
  /** The card's own `state` string, when it recorded one. */
  state: string | null;
}

/** What the open conversation's thread read knows about artifact cards. */
export type OutputRead =
  | { kind: "absent" }
  | { kind: "loading" }
  | { kind: "error"; error: string }
  | { kind: "withheld" }
  | { kind: "ready"; entries: ThreadEntry[] };

/** Live agent aliases on an operator DM or an agent-to-agent pair.
 *  A group has no single assignee; its roster stays the participants list. */
export function conversationAssignees(row: ConversationRow): string[] {
  if (row.kind !== "operator" && row.kind !== "pair") return [];
  const aliases: string[] = [];
  for (const participant of row.participants) {
    if (participant.kind !== "agent" || participant.removed || !participant.alias) continue;
    if (!aliases.includes(participant.alias)) aliases.push(participant.alias);
  }
  return aliases;
}

/** Every task on the assignee, including terminal ones. One done task is
 *  not folded into a conversation-level or job-level success. */
export function assigneeTasks(agent: Pick<Agent, "tasks">): ContextTask[] {
  const tasks = Array.isArray(agent.tasks) ? agent.tasks : [];
  return tasks.map((task) => ({
    id: task.task,
    title: taskTitle(task),
    state: task.task_state,
  }));
}

function taskTitle(task: AgentTask): string {
  const title = typeof task.title === "string" ? task.title.trim() : "";
  return title !== "" ? title : task.task;
}

/** Issue ids the roster already lists on `on`. Text in a title is not an id. */
export function assigneeIssues(agent: Pick<Agent, "on">): string[] {
  return agentOn(agent).filter((id) => id.trim() !== "");
}

/** Artifact cards on the loaded thread. An invalid card stays unavailable. */
export function conversationOutputs(entries: readonly ThreadEntry[]): ContextOutput[] {
  const outputs: ContextOutput[] = [];
  for (const entry of entries) {
    if (entry.kind !== "artifact") continue;
    const raw = entry.payload?.state;
    outputs.push({
      key: String(entry.seq),
      ref: artifactRef(entry.payload),
      state: typeof raw === "string" && raw.trim() !== "" ? raw : null,
    });
  }
  return outputs;
}

/** How the open canvas should follow a Threads conversation change.
 *  `keep` leaves a canvas this page does not own. `reset` drops the
 *  previous conversation's preview. `rebind` points Ask at the current
 *  composer draft without clearing tabs. */
export type CanvasFollow = "keep" | "reset" | "rebind";

export function canvasFollow(
  canvas: { conversation: string | null; composerKey: string | null },
  previousConversation: string | null,
  nextConversation: string | null,
  nextComposerKey: string | null,
  /** True only after this page opened an artifact. A matching id is not ownership. */
  ownedByThreads: boolean,
): CanvasFollow {
  if (!ownedByThreads || !previousConversation || canvas.conversation !== previousConversation) return "keep";
  if (nextConversation !== previousConversation) return "reset";
  if (canvas.composerKey !== nextComposerKey) return "rebind";
  return "keep";
}

export function sameOutputRead(a: OutputRead, b: OutputRead): boolean {
  if (a.kind !== b.kind) return false;
  if (a.kind === "error" && b.kind === "error") return a.error === b.error;
  if (a.kind === "ready" && b.kind === "ready") return a.entries === b.entries;
  return true;
}
