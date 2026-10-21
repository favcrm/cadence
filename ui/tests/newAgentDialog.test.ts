import { buildBody } from "../src/features/agents/agentSetup";

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

// Cloud body pins no provider, endpoint kind, profile override or company.
const cloud = buildBody("cloud", emptyFields);
check("body" in cloud, "cloud body should build");
if ("body" in cloud) {
  check(cloud.body.location === "cloud", "cloud body location");
  check(cloud.body.provider === undefined, "cloud body must not send provider");
  check(cloud.body.endpoint_kind === undefined, "cloud body must not send endpoint_kind");
  check(cloud.body.params === "{}", "cloud body params");
}

// Local still requires the explicit pair.
check("error" in buildBody("local", emptyFields), "local without provider/endpoint must be rejected");
