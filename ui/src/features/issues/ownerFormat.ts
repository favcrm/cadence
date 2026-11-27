import type { AgentProfile } from "../../lib/generated/board-api";
import type { SelectOption } from "../../ui/Select";
import { slugifyHandle } from "../mentions/mentions";
import type { HumanPerson } from "../threads/humanThreadsClient";
import { selectCloudSpecialists } from "./hostedDispatchModel";

/** Format stored ticket owners (`agents/<id>` -> `@alias`, `@handle` unchanged). */
export function formatTicketOwner(
  owner: string | null | undefined,
  profiles?: readonly AgentProfile[] | null,
): string | null {
  if (!owner) return null;
  if (owner.startsWith("agents/")) {
    const profileId = owner.slice("agents/".length);
    const profile = profiles?.find((p) => p.id === profileId);
    if (profile?.alias) return `@${profile.alias}`;
  }
  return owner;
}

/** Build searchable Select options combining Unassigned, workspace teammates, and hired cloud agents. */
export function buildOwnerSelectOptions(input: {
  currentOwner: string | null | undefined;
  people: readonly HumanPerson[];
  profiles?: readonly AgentProfile[] | null;
  t: (key: string) => string;
}): SelectOption[] {
  const { currentOwner, people, profiles, t } = input;
  const unassigned: SelectOption = { value: "", label: t("Unassigned") };

  const seenHandles = new Set<string>();
  const humanOptions: SelectOption[] = [];
  for (const person of people) {
    const explicit = (person as HumanPerson & { handle?: string }).handle?.trim();
    const handle = explicit || slugifyHandle(person.name) || person.subject;
    if (!handle) continue;
    const value = `@${handle}`;
    const key = value.toLowerCase();
    if (seenHandles.has(key)) continue;
    seenHandles.add(key);
    humanOptions.push({
      value,
      label: value,
      ...(person.name && person.name !== value ? { hint: person.name } : {}),
      badge: t("Human"),
      group: t("Teammates"),
    });
  }
  humanOptions.sort((a, b) => a.label.localeCompare(b.label));

  const seenAgents = new Set<string>();
  const agentOptions: SelectOption[] = [];
  for (const profile of selectCloudSpecialists(profiles ?? undefined)) {
    const value = `agents/${profile.id}`;
    if (seenAgents.has(value)) continue;
    seenAgents.add(value);
    agentOptions.push({
      value,
      label: `@${profile.alias}`,
      ...(profile.role ? { hint: profile.role } : {}),
      badge: t("Agent"),
      group: t("Agents"),
    });
  }
  agentOptions.sort((a, b) => a.label.localeCompare(b.label));

  const out: SelectOption[] = [unassigned];
  if (
    currentOwner &&
    !humanOptions.some((o) => o.value === currentOwner) &&
    !agentOptions.some((o) => o.value === currentOwner)
  ) {
    out.push({
      value: currentOwner,
      label: formatTicketOwner(currentOwner, profiles) ?? currentOwner,
    });
  }
  out.push(...humanOptions, ...agentOptions);
  return out;
}
