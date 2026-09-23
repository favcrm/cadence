import {
  agentsEmptyCopy,
  NEED_SECTION_LABEL,
  NEEDS_DECISION_EMPTY,
  needLabel,
  notesStatusSentence,
  shaCiLabel,
} from "./uxCopy";

function equal(actual: unknown, expected: unknown): void {
  if (actual !== expected) {
    throw new Error(`expected ${String(expected)}, got ${String(actual)}`);
  }
}

// All-projects empty fleet keeps the fleet-empty sentence.
equal(agentsEmptyCopy("all"), "No agents registered.");

// A selected project with no bound agent does not claim the fleet is empty.
equal(agentsEmptyCopy("cadence"), "No agents bound to this project.");
if (agentsEmptyCopy("cadence").includes("registered")) {
  throw new Error("selected-project empty copy claimed the fleet is empty");
}

equal(needLabel("approval_menu"), "approval menu");
equal(needLabel("not_a_kind"), "unknown / unclassified");
equal(needLabel("approval"), "approval");
equal(needLabel("inbox_stale"), "stale inbox");
// CAD-253: the audience comes from the server; the UI keeps only copy.
equal(NEED_SECTION_LABEL.operator, "Needs your decision");
equal(NEEDS_DECISION_EMPTY, "Nothing needs your decision");

// CAD-267: main CI labels never launder a cancelled or missing SHA.
equal(needLabel("ci_unverified"), "ci unverified");
const covered = "3".repeat(40);
equal(shaCiLabel({ state: "passed" }), "passed");
equal(shaCiLabel({ state: "cancelled", covered_by: covered }), "cancelled — covered by 3333333");
equal(shaCiLabel({ state: "cancelled", covered_by: null }), "cancelled — not yet covered");
equal(shaCiLabel({ state: "missing", covered_by: covered }), "no ci run — covered by 3333333");
equal(shaCiLabel({ state: "cancelled", conclusion: "skipped" }), "skipped — not yet covered");
equal(shaCiLabel({ state: "failed", conclusion: "timed_out" }), "failed (timed_out)");
for (const state of ["cancelled", "missing", "pending", "failed"]) {
  if (shaCiLabel({ state, covered_by: covered }).includes("passed")) {
    throw new Error(`${state} SHA read as passed`);
  }
}

const notes = notesStatusSentence("notes", "qa", "doing");
if (notes == null || !notes.includes("bound job")) {
  throw new Error(`notes sentence should mention a bound job, got ${String(notes)}`);
}
if (notes.includes("M3") || /ship/i.test(notes)) {
  throw new Error(`notes sentence mentioned a future release: ${notes}`);
}
equal(notesStatusSentence("file", "qa", "doing"), null);
equal(notesStatusSentence("job", "qa", "doing"), null);

console.log("ux copy checks passed");
