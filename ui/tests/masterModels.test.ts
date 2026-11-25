import {
  effortMeterBars,
  effortShortLabel,
  filterModelOptions,
  masterModelsOptions,
  offeredCommands,
  pickLanded,
  restingStatus,
  SLASH_COMMANDS,
} from "../src/features/home/master";
import type { MasterModels } from "../src/lib/types";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

// CAD-574 — the dropdowns' read of `GET /api/master/models` (CAD-575's
// contract): ids and labels, cost tiers verbatim, current marked,
// efforts listed in order.
{
  const v = masterModelsOptions({
    current: { model: "claude-opus-4-6", effort: "high" },
    models: [
      { id: "claude-opus-4-6", label: "Claude Opus 4.6", cost_tier: "Paid", allowed_for: ["master"] },
      { id: "gpt-5-mini", label: "GPT-5 mini", cost_tier: "Low cost", allowed_for: ["master", "worker"] },
      { id: "pi-free", cost_tier: "Free" },
    ],
    efforts: ["low", "high", "max"],
  });
  equal(
    v.models.map((m) => [m.id, m.label, m.cost]),
    [
      ["claude-opus-4-6", "Claude Opus 4.6", "Paid"],
      ["gpt-5-mini", "GPT-5 mini", "Low cost"],
      ["pi-free", "pi-free", "Free"],
    ],
    "rows: id, label (falls back to id), cost",
  );
  equal(v.models[0].roles, ["master"], "allowed_for kept");
  equal([v.model, v.effort], ["claude-opus-4-6", "high"], "current marked");
  equal(v.efforts, ["low", "high", "max"], "efforts in order");
}

// Degrade, don't break: the refused/absent read (a read-only board, an
// older daemon without the route) is a null payload — the dropdown
// shows its empty state instead of dead rows; partial entries are
// pruned, not trusted.
{
  const v = masterModelsOptions(null);
  equal([v.models.length, v.efforts.length, v.model, v.effort], [0, 0, null, null], "refusal → empty");

  const partial = masterModelsOptions({
    models: [
      { label: "no id — dropped" },
      { id: "m1" },
      "not an object",
    ] as unknown as MasterModels["models"],
    efforts: ["low", "", 3 as unknown as string],
  });
  equal(partial.models.map((m) => [m.id, m.cost]), [["m1", "unknown"]], "bad rows pruned");
  equal(partial.efforts, ["low"], "bad efforts pruned");
}

// The pending spin clears when the session reports the pick — the
// pick list speaks `provider/id` (`fake/model-2`) while `master_state`
// may answer the provider's bare `model-2`; an unrelated report never
// lands it, and neither does an absent one.
{
  equal(pickLanded("model", "fake/model-2", "model-2"), true, "provider/id pick, bare-id report");
  equal(pickLanded("model", "fake/model-2", "fake/model-2"), true, "exact report");
  equal(pickLanded("model", "fake/model-2", "model-1"), false, "different model");
  equal(pickLanded("model", "fake/model-2", null), false, "nothing reported yet");
  equal(pickLanded("effort", "high", "high"), true, "effort exact");
  equal(pickLanded("effort", "xhigh/high", "high"), false, "effort has no provider/id form");
}

// CAD-2013: the header's word for an agent with no turn on this surface.
equal(restingStatus("idle"), "Available", "idle is available");
equal(restingStatus("running"), "Available", "a live session between turns is available");
equal(restingStatus("busy"), "Working", "busy is real work, never available");
equal(restingStatus("waiting_input"), null, "an unlisted state is not rounded up to healthy");

// CAD-2027: a hosted board offers what it says it can run, and help.
{
  const names = (hosted: boolean | null, advertised?: string[]) => offeredCommands(hosted, advertised).map((c) => c.name);
  equal(names(true, []), ["help"], "a hosted board advertising nothing offers help alone");
  equal(names(true), ["help"], "a hosted board whose state is unread offers help alone");
  equal(names(true, ["stop", "new", "forged"]), ["help", "new", "stop"], "a hosted board offers known verbs it advertises");
  equal(names(false, []).length, SLASH_COMMANDS.length, "a native board keeps the full list whatever its provider advertises");
  equal(
    names(null, []),
    SLASH_COMMANDS.filter((c) => c.name !== "permissions").map((c) => c.name),
    "an unknown board keeps non-native-only commands",
  );
}

// CAD-2792: default flag, per-model efforts, filterModelOptions, and 3-bar effort meter helpers.
{
  const parsed = masterModelsOptions({
    current: { model: "thinkingmachines/inkling-2-mini", effort: "medium" },
    models: [
      {
        id: "deepseek/deepseek-v4.1-flash-1017",
        label: "DeepSeek V4.1 Flash",
        is_default: true,
        efforts: [],
        default_effort: null,
      },
      {
        id: "thinkingmachines/inkling-2-mini",
        label: "Inkling 2 Mini",
        is_default: false,
        efforts: ["low", "medium", "high"],
        default_effort: "low",
      },
    ],
    efforts: ["low", "medium", "high"],
  });
  equal(parsed.models[0].isDefault, true, "is_default parsed");
  equal(parsed.models[0].efforts, [], "empty efforts preserved");
  equal(parsed.models[1].efforts, ["low", "medium", "high"], "per-model efforts preserved");
  equal(parsed.models[1].defaultEffort, "low", "default_effort parsed");

  equal(filterModelOptions(parsed.models, "").length, 2, "blank query keeps all");
  equal(filterModelOptions(parsed.models, "inkling").map((m) => m.id), ["thinkingmachines/inkling-2-mini"], "matches label");
  equal(filterModelOptions(parsed.models, "v4.1").map((m) => m.id), ["deepseek/deepseek-v4.1-flash-1017"], "matches id");
  equal(filterModelOptions(parsed.models, "missing"), [], "no match returns empty");

  equal(effortMeterBars("none"), 0, "none -> 0 bars");
  equal(effortMeterBars("minimal"), 1, "minimal -> 1 bar");
  equal(effortMeterBars("low"), 1, "low -> 1 bar");
  equal(effortMeterBars("medium"), 2, "medium -> 2 bars");
  equal(effortMeterBars("high"), 3, "high -> 3 bars");
  equal(effortMeterBars("max"), 3, "max -> 3 bars");

  equal(effortShortLabel("none"), "Off", "none short label");
  equal(effortShortLabel("minimal"), "Min", "minimal short label");
  equal(effortShortLabel("low"), "Low", "low short label");
  equal(effortShortLabel("medium"), "Med", "medium short label");
  equal(effortShortLabel("high"), "High", "high short label");
  equal(effortShortLabel("max"), "Max", "max short label");
}

console.log("master models checks passed");
