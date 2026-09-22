import { provenanceDetail } from "./modelProvenance";

function equal(actual: unknown, expected: unknown): void {
  if (actual !== expected) {
    throw new Error(`expected ${String(expected)}, got ${String(actual)}`);
  }
}

equal(
  provenanceDetail({ source: "provider_baseline", lookup_role: "worker", revision: 0 }),
  "provider baseline · lookup worker · revision 0",
);
equal(
  provenanceDetail({ source: "role_default", revision: 3 }, "qa"),
  "role default · lookup qa · revision 3",
);
equal(
  provenanceDetail({ source: "explicit", lookup_role: "qa", revision: null }),
  "explicit launch model · lookup qa",
);
equal(
  provenanceDetail({ source: "legacy_configured", revision: null }),
  "saved before model defaults",
);
equal(provenanceDetail(null), null);

console.log("model provenance checks passed");
