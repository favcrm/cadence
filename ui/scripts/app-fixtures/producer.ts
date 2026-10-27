import { screenProjection } from "../../src/features/workspace-apps/screen/screenProjection";
import { shapeFor } from "../../src/features/workspace-apps/screen/screenProtocol";
import type { IntentRead, ScreenExtras } from "../../src/features/workspace-apps/screen/screenProjection";
import type { AppContext, AppEffect, Installation, SlotDeclaration, WorkspaceRun } from "../../src/features/workspace-apps/workspaceApps";

declare const process: {
  stdin: AsyncIterable<Uint8Array>;
  stdout: { write(value: string): unknown };
  stderr: { write(value: string): unknown };
  exitCode?: number;
};

const MAX_INPUT = 1024 * 1024;
const MAX_OUTPUT = 8 * 1024 * 1024;
const CONTRACT = "app-screen-projection-fixtures/4";
const NOW = 1_799_999_000;
const SCENARIOS = ["empty", "loading", "unavailable", "ready", "draft", "review"] as const;
type Scenario = typeof SCENARIOS[number];

type Input = {
  contract: string;
  bundle_digest: string;
  manifest: { app: string; title: string; version: string; summary: string | null; capabilities: Record<string, SlotDeclaration> };
  inventory: string[];
  screen: { tag: string; contract: string; may: string[]; tools: Record<string, string> };
  kit: string;
};

function exactKeys(value: unknown, keys: string[], label: string): asserts value is Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value) ||
      Object.keys(value).sort().join("\0") !== [...keys].sort().join("\0"))
    throw new Error(`invalid closed ${label} metadata`);
}

function runFor(installation: Installation, id: string, mode: "ready" | "draft" | "review"): WorkspaceRun {
  const contextId = "fixture-context";
  const running = mode !== "ready";
  const state = running ? "running" : "succeeded";
  const stepKind = mode === "review" ? "review_text" : "produce_text";
  return {
    id, install_id: installation.install_id, context_id: contextId, state,
    snapshot_digest: installation.digest, approved_digest: null,
    snapshot: {
      workflow: { title: mode === "ready" ? "Synthetic completed run" : "Synthetic draft",
        steps: [{ id: "fixture-step", kind: stepKind, assignee: "fixture", dependencies: [], instruction: "Synthetic fixture only" }] },
      inputs: {}, owner_pm: "", assignments: {}, capabilities: {},
      context: { id: contextId, revision: 1, digest: installation.digest },
      publication: { slot: "", binding: null },
    },
    steps: [{ step_id: "fixture-step", task_id: `task-${id}`,
      state: mode === "ready" ? "succeeded" : mode === "review" ? "dispatched" : "pending", message_id: null }],
    created: NOW - 60, updated: NOW, artifacts: [], reviews: [],
  };
}

function makeInstallation(input: Input): Installation {
  const capabilities: Installation["capabilities"] = Object.fromEntries(Object.entries(input.manifest.capabilities)
    .map(([name, declaration]) => [name, { ...declaration }])) as Installation["capabilities"];
  return {
    install_id: "fixture-install", name: input.manifest.app, title: input.manifest.title,
    version: input.manifest.version, summary: input.manifest.summary ?? "", digest: input.bundle_digest,
    catalog_generation: "fixture-catalog", storage_kind: "workspace", project_link: null,
    approved: false, executable: true, approval: { state: "synthetic" }, guide: "",
    files: [...input.inventory], capabilities, connection_slots: [], workflows: [],
  };
}

function makeContext(installation: Installation): AppContext {
  return { id: "fixture-context", install_id: installation.install_id, revision: 1,
    digest: installation.digest, state: "active",
    config: { schema: 1, label: "Synthetic context", input_defaults: {} } };
}

function makeExtras(installation: Installation, contextId: string): ScreenExtras {
  return { installId: installation.install_id, contextId, bindings: [], workers: null, texts: new Map() };
}

function makeScenario(input: Input, installation: Installation, context: AppContext, label: Scenario) {
  const contextId = label === "empty" ? "" : context.id;
  const contexts = label === "empty" ? [] : [context];
  const runs: WorkspaceRun[] = label === "ready" || label === "draft" || label === "review"
    ? [runFor(installation, `fixture-${label}`, label)] : [];
  const effects: AppEffect[] = [];
  const extras = makeExtras(installation, contextId);
  const intents: IntentRead | undefined = label === "loading" ? undefined : {
    installId: installation.install_id, contextId,
    status: label === "unavailable" ? "unavailable" : "ok", intents: [],
  };
  const projection = screenProjection(installation, input.screen.tag, contextId, contexts, runs, effects, intents, extras);
  const push = shapeFor(projection, "v2");
  if (!push) throw new Error(`canonical v2 shape refused fixture '${label}'`);
  return { label, push };
}

function validate(input: unknown): asserts input is Input {
  exactKeys(input, ["contract", "bundle_digest", "manifest", "inventory", "screen", "kit"], "producer input");
  const value = input as Input;
  if (value.contract !== CONTRACT || typeof value.bundle_digest !== "string" ||
      !/^sha256:[0-9a-f]{64}$/.test(value.bundle_digest) || typeof value.kit !== "string" ||
      !Array.isArray(value.inventory) || value.inventory.some(path => typeof path !== "string") ||
      typeof value.manifest.app !== "string" || typeof value.manifest.title !== "string" ||
      typeof value.manifest.version !== "string" ||
      !(value.manifest.summary === null || typeof value.manifest.summary === "string") ||
      !value.manifest.capabilities || typeof value.manifest.capabilities !== "object" ||
      Array.isArray(value.manifest.capabilities))
    throw new Error("invalid fixture metadata");
  exactKeys(value.manifest, ["app", "title", "version", "summary", "capabilities"], "manifest");
  exactKeys(value.screen, ["tag", "contract", "may", "tools"], "screen");
  if (value.screen.contract !== "app-screens/v2" || typeof value.screen.tag !== "string" ||
      !Array.isArray(value.screen.may) || !value.screen.tools || typeof value.screen.tools !== "object" ||
      Array.isArray(value.screen.tools))
    throw new Error("unsupported or mismatched screen contract");
  for (const declaration of Object.values(value.manifest.capabilities)) {
    exactKeys(declaration, ["schema", "capability", "version", "action", "resource_kind", "effect"], "capability slot");
  }
}

async function main() {
  const chunks: Uint8Array[] = [];
  let inputBytes = 0;
  for await (const chunk of process.stdin) {
    inputBytes += chunk.byteLength;
    if (inputBytes > MAX_INPUT) throw new Error("fixture input exceeds bound");
    chunks.push(chunk);
  }
  const bytes = new Uint8Array(inputBytes);
  let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
  const input: unknown = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
  validate(input);
  const oldNow = Date.now;
  Date.now = () => NOW * 1000;
  try {
    const installation = makeInstallation(input);
    const context = makeContext(installation);
    const scenarios = SCENARIOS.map(label => makeScenario(input, installation, context, label));
    const result = { fixture: true, contract: CONTRACT, kit: input.kit,
      screen_contract: input.screen.contract, bundle_digest: input.bundle_digest,
      screen: { tag: input.screen.tag, app: input.manifest.app, may: input.screen.may, tools: input.screen.tools },
      scenarios };
    const output = JSON.stringify(result);
    if (new TextEncoder().encode(output).byteLength > MAX_OUTPUT) throw new Error("fixture output exceeds bound");
    process.stdout.write(output);
  } finally { Date.now = oldNow; }
}

main().catch(error => { process.stderr.write(`fixture producer refused: ${String(error?.message ?? "error").slice(0, 512)}\n`); process.exitCode = 1; });
