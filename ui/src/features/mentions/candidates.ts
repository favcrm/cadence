import type { KeyboardEvent } from "react";
import type { Agent } from "../../lib/types";
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

/** Build shared `@mention` picker candidates combining shareable agents and workspace teammates. */
export function buildMentionCandidates(
  agents: readonly Agent[] | undefined,
  teammates: readonly HumanPerson[],
  teammateSubtitle: string,
  excludeAlias?: string | null,
): MentionCandidate[] {
  return [
    ...(agents ?? [])
      .filter((a) => isShareableAgent(a, excludeAlias))
      .map((a) => ({
        id: a.alias,
        handle: a.alias,
        label: a.alias,
        kind: "agent" as const,
        subtitle: a.role ?? undefined,
        active: true,
      })),
    ...teammateMentionCandidates(teammates, teammateSubtitle),
  ];
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
