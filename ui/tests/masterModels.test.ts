import { masterModelsOptions, pickLanded } from "../src/features/home/master";
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

console.log("master models checks passed");
