import { agentsEmptyCopy, needGroupKey, needLabel, notesStatusSentence } from "./uxCopy";

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
equal(needGroupKey("approval_menu"), "team");
equal(needGroupKey("approval"), "decision");
equal(needGroupKey("not_a_kind"), "team");
equal(needLabel("inbox_stale"), "stale inbox");

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
