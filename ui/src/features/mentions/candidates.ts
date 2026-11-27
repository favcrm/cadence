import type { KeyboardEvent } from "react";
import type { Agent, HiredProfile } from "../../lib/types";
import type { HumanPerson } from "../threads/humanThreadsClient";
import type { MentionCandidate } from "./mentions";
import { teammateMentionCandidates } from "./useWorkspacePeople";

/**
 * Exclude personal assistants, `master`, `reviewer`, `inbox`, and offline/dead
 * agents from shared `@mention` and channel-picker surfaces.
 */
export function isShareableAgent(
  agent: Pick<Agent, "alias" | "inbox" | "provider" | "dead" | "role" | "personal_assistant">,
  excludeAlias?: string | null,
): boolean {
  const aliasLower = agent.alias.trim().toLowerCase();
  return (
    !agent.personal_assistant &&
    !agent.inbox &&
    agent.provider !== "inbox" &&
    !agent.dead &&
    aliasLower !== "master" &&
    !aliasLower.startsWith("assist-") &&
    (agent.role ?? "").toLowerCase() !== "reviewer" &&
    (!excludeAlias || agent.alias !== excludeAlias)
  );
}

/** Fields required to determine and render an agent `@mention` candidate. */
export type MentionAgentInput = Pick<Agent, "alias" | "inbox" | "provider" | "dead" | "role" | "personal_assistant">;

/** Fields required to determine and render a hosted hired-profile `@mention` candidate. */
export type MentionProfileInput = Pick<HiredProfile, "id" | "alias" | "role" | "team_role"> & {
  status?: string | null;
};

/** Exclude `master`, `assist-*`, `reviewer`, `archived`, and self from hosted hired profiles. */
export function isShareableProfile(
  profile: Pick<HiredProfile, "alias" | "role" | "team_role"> & { status?: string | null },
  excludeAlias?: string | null,
): boolean {
  const aliasLower = profile.alias.trim().toLowerCase();
  return (
    aliasLower !== "" &&
    aliasLower !== "master" &&
    !aliasLower.startsWith("assist-") &&
    (profile.role ?? "").toLowerCase() !== "reviewer" &&
    (profile.team_role ?? "").toLowerCase() !== "reviewer" &&
    (profile.status ?? "").toLowerCase() !== "archived" &&
    (!excludeAlias || profile.alias !== excludeAlias)
  );
}

/** Build shared `@mention` picker candidates combining shareable agents, hosted profiles, and workspace teammates. */
export function buildMentionCandidates(
  agents: readonly MentionAgentInput[] | undefined,
  teammates: readonly HumanPerson[],
  teammateSubtitle: string,
  excludeAlias?: string | null,
  profiles?: readonly MentionProfileInput[] | undefined,
): MentionCandidate[] {
  const out: MentionCandidate[] = [];
  const seenAliases = new Set<string>();
  for (const a of agents ?? []) {
    if (!isShareableAgent(a, excludeAlias)) continue;
    const key = a.alias.trim().toLowerCase();
    if (!key || seenAliases.has(key)) continue;
    seenAliases.add(key);
    out.push({
      id: a.alias,
      handle: a.alias,
      label: a.alias,
      kind: "agent",
      subtitle: a.role ?? undefined,
      active: true,
    });
  }
  for (const p of profiles ?? []) {
    if (!isShareableProfile(p, excludeAlias)) continue;
    const key = p.alias.trim().toLowerCase();
    if (seenAliases.has(key)) continue;
    seenAliases.add(key);
    out.push({
      id: p.id,
      handle: p.alias,
      label: p.alias,
      kind: "agent",
      subtitle: p.role || undefined,
      active: true,
    });
  }
  out.push(...teammateMentionCandidates(teammates, teammateSubtitle));
  return out;
}

/** Shared keyboard navigation for `@mention` pickers inside textarea inputs. */
export function handleMentionKeyDown(
  event: KeyboardEvent<HTMLTextAreaElement>,
  opts: {
    matches: readonly MentionCandidate[];
    selectableIndices: readonly number[];
    highlightedIndex: number;
    onHighlight: (index: number) => void;
    onSelect: (candidate: MentionCandidate) => void;
    onDismiss: () => void;
  },
): boolean {
  const { matches, selectableIndices, highlightedIndex, onHighlight, onSelect, onDismiss } = opts;
  if (matches.length === 0) return false;
  if (event.key === "ArrowDown" && selectableIndices.length > 0) {
    event.preventDefault();
    const curPos = selectableIndices.indexOf(highlightedIndex);
    const nextPos = curPos < 0 ? 0 : Math.min(curPos + 1, selectableIndices.length - 1);
    onHighlight(selectableIndices[nextPos]!);
    return true;
  }
  if (event.key === "ArrowUp" && selectableIndices.length > 0) {
    event.preventDefault();
    const curPos = selectableIndices.indexOf(highlightedIndex);
    const prevPos = curPos <= 0 ? 0 : curPos - 1;
    onHighlight(selectableIndices[prevPos]!);
    return true;
  }
  if (event.key === "Escape") {
    event.preventDefault();
    event.stopPropagation();
    onDismiss();
    return true;
  }
  if (highlightedIndex >= 0 && (event.key === "Tab" || (event.key === "Enter" && !event.shiftKey))) {
    event.preventDefault();
    const picked = matches[highlightedIndex];
    if (picked) onSelect(picked);
    return true;
  }
  return false;
}
