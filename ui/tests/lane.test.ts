import {
  branchTitle,
  composerBlocked,
  costLabel,
  isStatusAsk,
  effortChoices,
  modelChoices,
  reassignBody,
  statusText,
  unfenceReady,
} from "../src/features/issues/lane";

function equal(actual: unknown, expected: unknown): void {
  const a = JSON.stringify(actual);
  const b = JSON.stringify(expected);
  if (a !== b) throw new Error(`expected ${b}, got ${a}`);
}

equal(statusText(""), "status?");
equal(statusText("  where is the PR  "), "status? where is the PR");
equal(statusText(null), "status?");

equal(isStatusAsk("nudge", "look here"), true);
equal(isStatusAsk("operator", "status?"), true);
equal(isStatusAsk("operator", "ship the fix"), false);

equal(unfenceReady(null), false);
equal(unfenceReady(""), false);
equal(unfenceReady("interrupted"), true);
equal(unfenceReady("completed"), true);
equal(unfenceReady("failed"), true);
equal(unfenceReady("done"), false);

equal(composerBlocked("busy"), false);
equal(composerBlocked("idle"), false);
equal(composerBlocked("fenced"), true);
equal(composerBlocked("quota"), true);
equal(composerBlocked("rate-limited"), true);
equal(composerBlocked("shipped"), true);
equal(composerBlocked(null), true);

equal(branchTitle("cadence/cad-608-lane"), "cadence/cad-608-lane");
equal(branchTitle(null), "");

equal(costLabel("cursor", "grok-4.7-high"), "Cursor plan");
equal(costLabel("devin", "swe-2-high"), "Free (quota)");
equal(costLabel("pi", "devin/swe-2-max"), "Free (quota)");
equal(costLabel("openrouter", "openrouter/example-flash"), "Paid");
equal(costLabel("pi", "devin/deepseek-v4"), "Paid");
equal(costLabel("fake", ""), "");

equal(modelChoices("pi", ["devin/swe-2-high", "devin/deepseek-v4"]), [
  { value: "", label: "Select" },
  { value: "devin/swe-2-high", label: "devin/swe-2-high", badge: "Free (quota)" },
  { value: "devin/deepseek-v4", label: "devin/deepseek-v4", badge: "Paid" },
]);
equal(modelChoices("cursor", ["grok-4.7-high"]), [
  { value: "", label: "Select" },
  { value: "grok-4.7-high", label: "grok-4.7-high", badge: "Cursor plan" },
]);
equal(effortChoices(["low", "max"]), [
  { value: "", label: "Default" },
  { value: "low", label: "low" },
  { value: "max", label: "max" },
]);

equal(reassignBody({ provider: "fake", model: "", effort: "", note: "  " }), { provider: "fake" });
equal(
  reassignBody({ provider: "pi", model: "devin/swe-2-high", effort: "max", note: "keep going" }),
  { provider: "pi", model: "devin/swe-2-high", effort: "max", note: "keep going" },
);

console.log("lane checks passed");
