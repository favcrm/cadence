import { MASTER } from "../home/master";
import type { Agent, AgentsPayload, PersonalAssistantRow } from "../../lib/types";

/** CAD-2353: the personal assistant's page is `/agents/assistant`. The route
 *  is resolved by scope, so a hired profile that is also named `assistant`
 *  never takes it while the roster carries a personal row. */
export const PERSONAL_ALIAS = "assistant";

export function personalOf(payload: AgentsPayload | null | undefined): PersonalAssistantRow | null {
  const row = payload?.personal_assistant;
  return row && row.scope === "personal" ? row : null;
}

/** The company Master, as the newer hosted roster labels it. An older API sends
 *  neither field, so the row renders as it always did. */
export function isCompanyMaster(a: Agent, payload: AgentsPayload | null | undefined): boolean {
  return a.alias === MASTER && (a.scope === "workspace" || personalOf(payload) !== null);
}

export function hasScopedAssistantRoster(payload: AgentsPayload): boolean {
  return Object.hasOwn(payload, "personal_assistant") || payload.agents.some((a) => isCompanyMaster(a, payload));
}

/** The company Master is for owners: on a hosted board a roster that names the
 *  personal assistant does not list it to anyone else (an unanswered access
 *  check is not an owner). Native boards and older APIs keep every row. */
export function masterShownTo(
  a: Agent,
  payload: AgentsPayload | null | undefined,
  hostedBoard: boolean,
  operator: boolean | null,
): boolean {
  return !(hostedBoard && isCompanyMaster(a, payload) && operator !== true);
}

export const MASTER_LABEL = "Master · company, owners only";
