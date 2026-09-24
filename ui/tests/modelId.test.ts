import { modelIdProblem } from "../src/features/settings/modelId";

function equal(actual: unknown, expected: unknown): void {
  if (actual !== expected) {
    throw new Error(`expected ${String(expected)}, got ${String(actual)}`);
  }
}

equal(modelIdProblem("  "), "Enter a model id.");
equal(modelIdProblem("a".repeat(200)), null);
equal(modelIdProblem("a".repeat(201)), "Model ids are at most 200 bytes.");
equal(modelIdProblem("é".repeat(100)), null);
equal(
  modelIdProblem("é".repeat(101)),
  "Model ids are at most 200 bytes.",
);
equal(modelIdProblem("ok\u007f"), "Model ids cannot contain control characters.");
equal(modelIdProblem("ok\u0085"), "Model ids cannot contain control characters.");
equal(modelIdProblem("opus"), null);

console.log("model id checks passed");
