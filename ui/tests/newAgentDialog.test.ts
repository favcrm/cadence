import { buildBody, cloudParams } from "../src/features/agents/agentSetup";

function check(condition: boolean, message: string): void {
  if (!condition) throw new Error(`newAgentDialog: ${message}`);
}

const emptyFields = {
  alias: "scout",
  provider: "",
  endpointKind: "",
  role: "",
  teamRole: "",
  modelPolicy: "",
  sandbox: "",
  instructions: "",
  cwd: "",
};

const empty = { image_profile: "", monthly_budget_micros: "" };

// Cloud params carry only the two AOS-accepted keys, both optional.
const none = cloudParams(empty);
check("params" in none && none.params === "{}", "empty cloud profile should be {}");

const full = cloudParams({ image_profile: "rust-lite", monthly_budget_micros: "25000000" });
check(
  "params" in full && full.params === JSON.stringify({ image_profile: "rust-lite", monthly_budget_micros: 25000000 }),
  "full cloud profile should map to image_profile and monthly_budget_micros",
);

// Image profile is limited to the two AOS values.
check("error" in cloudParams({ ...empty, image_profile: "node20" }), "unknown image profile must be rejected");

// Budget must be a positive whole number of micros.
for (const bad of ["0", "-5", "1.5", "abc", "1e3"]) {
  check("error" in cloudParams({ ...empty, monthly_budget_micros: bad }), `budget '${bad}' must be rejected`);
}

// Removed keys never leak into params.
const out = cloudParams({ image_profile: "node24-pi", monthly_budget_micros: "100" });
check("params" in out, "valid profile should produce params");
if ("params" in out) {
  check(
    Object.keys(JSON.parse(out.params)).sort().join(",") === "image_profile,monthly_budget_micros",
    "params must contain only the AOS-accepted keys",
  );
}

// Cloud body pins no provider or endpoint kind and carries no company.
const cloud = buildBody("cloud", emptyFields, empty);
check("body" in cloud, "cloud body should build");
if ("body" in cloud) {
  check(cloud.body.location === "cloud", "cloud body location");
  check(cloud.body.provider === undefined, "cloud body must not send provider");
  check(cloud.body.endpoint_kind === undefined, "cloud body must not send endpoint_kind");
  check(cloud.body.params === "{}", "cloud body params");
}

// Local still requires the explicit pair.
check("error" in buildBody("local", emptyFields, empty), "local without provider/endpoint must be rejected");
