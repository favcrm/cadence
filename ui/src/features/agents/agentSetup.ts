import type { AgentRegisterBody } from "../../lib/types";

export type Location = "local" | "cloud";

/** Build the POST body. Cloud sends no provider, endpoint kind or profile
 * overrides; the hosted workspace binds the company and the daemon selects
 * the sandbox endpoint. Local sends the existing provider, endpoint kind
 * and cwd. */
export function buildBody(
  location: Location,
  fields: {
    alias: string;
    provider: string;
    endpointKind: string;
    role: string;
    teamRole: string;
    modelPolicy: string;
    sandbox: string;
    instructions: string;
    cwd: string;
  },
): { body: AgentRegisterBody } | { error: string } {
  const alias = fields.alias.trim();
  if (!alias) return { error: "An agent needs an alias." };
  const body: AgentRegisterBody = { alias, location };
  const opt = (value: string) => (value.trim() ? value.trim() : undefined);
  body.role = opt(fields.role);
  body.team_role = opt(fields.teamRole);
  body.model_policy = opt(fields.modelPolicy);
  body.sandbox = opt(fields.sandbox);
  body.instructions = opt(fields.instructions);
  if (location === "local") {
    if (!fields.provider.trim() || !fields.endpointKind.trim())
      return { error: "A local agent needs a provider and an endpoint kind." };
    body.provider = fields.provider.trim();
    body.endpoint_kind = fields.endpointKind.trim();
    body.cwd = opt(fields.cwd);
  } else {
    body.params = "{}";
  }
  return { body };
}
