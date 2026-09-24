import { canonicalJson } from "../src/features/settings/modelDefaultsCompare";

function equal(actual: unknown, expected: unknown): void {
  if (actual !== expected) {
    throw new Error(`expected ${String(expected)}, got ${String(actual)}`);
  }
}

const emptyEditor = { schema: 1, providers: {} };
const emptyServer = { providers: {}, schema: 1 };
equal(canonicalJson(emptyEditor), canonicalJson(emptyServer));

const editor = {
  schema: 1,
  providers: {
    claude: {
      default: { mode: "model", model: "baseline-a" },
      roles: {
        qa: { mode: "model", model: "qa-model" },
        dev: { mode: "provider_default" },
        pm: { mode: "model", model: "pm-model" },
      },
    },
  },
};
const server = {
  providers: {
    claude: {
      roles: {
        dev: { mode: "provider_default" },
        pm: { model: "pm-model", mode: "model" },
        qa: { model: "qa-model", mode: "model" },
      },
      default: { model: "baseline-a", mode: "model" },
    },
  },
  schema: 1,
};
equal(canonicalJson(editor), canonicalJson(server));

const changed = {
  schema: 1,
  providers: {
    claude: {
      default: { mode: "model", model: "baseline-b" },
      roles: {},
    },
  },
};
if (canonicalJson(editor) === canonicalJson(changed)) {
  throw new Error("a different model must stay dirty");
}

console.log("model defaults compare checks passed");
