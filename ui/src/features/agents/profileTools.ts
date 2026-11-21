import type { StoredAgentProfile } from "../../lib/types";

/** AOS-679: tool groups that are off unless a profile's `enabled_tools` names
 *  them (agenticos-v2 `OPT_IN_TOOL_GROUP_IDS`). Every other group is on unless
 *  `disabled_tools` lists it, so absence from `disabled_tools` must never read
 *  as "on" for one of these. */
export const OPT_IN_TOOL_GROUPS: { id: string; label: string; line: string }[] = [
  {
    id: "cadence_writes",
    label: "Cadence writes",
    line: "Lets this agent create cards, comment and attach in its project, and edit only cards it created",
  },
];

const isOptIn = (group: string): boolean => OPT_IN_TOOL_GROUPS.some((g) => g.id === group);

/** Whether `group` is on for this profile. */
export function toolGroupOn(profile: Pick<StoredAgentProfile, "disabled_tools" | "enabled_tools">, group: string): boolean {
  if (isOptIn(group)) return (profile.enabled_tools ?? []).includes(group);
  return !(profile.disabled_tools ?? []).includes(group);
}

/** The `enabled_tools` list after turning `group` on or off. */
export function withEnabledGroup(profile: Pick<StoredAgentProfile, "enabled_tools">, group: string, on: boolean): string[] {
  const rest = (profile.enabled_tools ?? []).filter((g) => g !== group);
  return on ? [...rest, group] : rest;
}
