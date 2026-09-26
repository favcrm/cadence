/** CAD-608 lane card helpers. No React — node tests import this file.
 *  Cost labels match the daemon's `cost_label` (lane_rpc.rs). */

export interface LanePr {
  url?: string | null;
  label?: string | null;
}

export interface LaneView {
  agent: string;
  provider: string;
  model: string | null;
  effort?: string | null;
  cost: string | null;
  branch: string | null;
  pr: LanePr | null;
  activity: { at: number; state: string } | null;
  state: LaneState | string;
  worktree?: string;
  group?: string;
}

export interface LaneProvider {
  id: string;
  model: boolean;
  effort: boolean;
  models: string[] | null;
  efforts: string[];
}

export interface LanePayload {
  issue: string;
  lane: LaneView | null;
  providers: LaneProvider[];
}

export type LaneState =
  | "busy"
  | "idle"
  | "fenced"
  | "quota"
  | "rate-limited"
  | "shipped"
  | "none";

/** The composer is off while the lane cannot take a turn. */
export function composerBlocked(state: string | null | undefined): boolean {
  return state === "fenced" || state === "quota" || state === "rate-limited" || state === "shipped" || !state || state === "none";
}

/** Ask for status: the server stores `status?` plus optional extra. */
export function statusText(extra?: string | null): string {
  const text = (extra ?? "").trim();
  if (!text) return "status?";
  return `status? ${text}`;
}

/** A thread row is an ask when the daemon nudged it, or the text is the template. */
export function isStatusAsk(source: unknown, text: string): boolean {
  if (source === "nudge") return true;
  return text.trimStart().startsWith("status?");
}

/** Unfence has no default. The button stays disabled until one of the three is chosen. */
export function unfenceReady(status: string | null | undefined): boolean {
  return status === "interrupted" || status === "completed" || status === "failed";
}

/** Full branch for the title attribute; the card ellipsizes the visible text. */
export function branchTitle(branch: string | null | undefined): string {
  return branch ?? "";
}

/** Cost chip. Cursor plan, free Devin quota, otherwise paid when a model is set. */
export function costLabel(provider: string, model: string | null | undefined): string {
  const id = (model ?? "").toLowerCase();
  if (provider === "openrouter" || id.startsWith("openrouter/")) return "Paid";
  if (id.includes("swe-2")) return "Free (quota)";
  if (provider === "cursor") return "Cursor plan";
  if (!id) return "";
  return "Paid";
}

/** A Select option. `badge` is the cost label shown beside a model. */
export interface SelectChoice {
  value: string;
  label: string;
  badge?: string;
}

/** Model list for the reassign Select. The blank row clears a chosen model. */
export function modelChoices(provider: string, models: readonly string[]): SelectChoice[] {
  return [
    { value: "", label: "Select" },
    ...models.map((id) => {
      const badge = costLabel(provider, id);
      return badge ? { value: id, label: id, badge } : { value: id, label: id };
    }),
  ];
}

/** Effort list for the reassign Select. The blank row is the provider default. */
export function effortChoices(efforts: readonly string[]): SelectChoice[] {
  return [{ value: "", label: "Default" }, ...efforts.map((id) => ({ value: id, label: id }))];
}

export interface ReassignDraft {
  provider: string;
  model: string;
  effort: string;
  note: string;
}

/** Body for `POST .../lane/reassign`. Empty model/effort/note are omitted. */
export function reassignBody(draft: ReassignDraft): {
  provider: string;
  model?: string;
  effort?: string;
  note?: string;
} {
  const body: { provider: string; model?: string; effort?: string; note?: string } = {
    provider: draft.provider,
  };
  const model = draft.model.trim();
  const effort = draft.effort.trim();
  const note = draft.note.trim();
  if (model) body.model = model;
  if (effort) body.effort = effort;
  if (note) body.note = note;
  return body;
}
