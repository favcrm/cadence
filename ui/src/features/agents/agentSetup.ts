import type { AgentRegisterBody } from "../../lib/types";

export type Location = "local" | "cloud";

/** Cloud profile: the only sandbox launch params AOS accepts. Company is
 * bound by the hosted workspace (never a caller field); the repo is derived
 * from the project. Sent in `params` (an opaque JSON string the daemon stores
 * with the row); the daemon itself sets provider and endpoint kind to `sandbox`. */
export interface CloudProfile {
  image_profile: string;
  monthly_budget_micros: string;
}

export const EMPTY_CLOUD: CloudProfile = {
  image_profile: "",
  monthly_budget_micros: "",
};

export const IMAGE_PROFILES = ["node24-pi", "rust-lite"] as const;

/** Validates the cloud profile and returns the `params` JSON, or a message.
 * Both keys are optional; the object is `{}` when neither is set. */
export function cloudParams(profile: CloudProfile): { params: string } | { error: string } {
  const image = profile.image_profile.trim();
  if (image !== "" && !(IMAGE_PROFILES as readonly string[]).includes(image))
    return { error: "Image profile must be node24-pi or rust-lite." };
  const budget = profile.monthly_budget_micros.trim();
  if (budget !== "" && !(/^[1-9][0-9]*$/.test(budget) && Number.isSafeInteger(Number(budget))))
    return { error: "Monthly budget must be a positive whole number of micros." };
  const params: Record<string, string | number> = {};
  if (image !== "") params.image_profile = image;
  if (budget !== "") params.monthly_budget_micros = Number(budget);
  return { params: JSON.stringify(params) };
}

/** Build the POST body. Cloud sends no provider or endpoint kind; local sends
 * the existing provider, endpoint kind and cwd. */
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
  cloud: CloudProfile,
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
    const profile = cloudParams(cloud);
    if ("error" in profile) return profile;
    body.params = profile.params;
  }
  return { body };
}
