import { evidenceSuffix, quorumLabel, quorumView } from "../src/features/settings/memoryView";
import type { MemoryCard, MemoryEvidence } from "../src/lib/types";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

const card = (evidence: MemoryEvidence | null, verified_at = "2026-09-23T00:00:00Z") =>
  ({
    status: "accepted",
    verified_at,
    quorum: { eligible: true, reason: "finalized" },
    evidence,
  }) as unknown as MemoryCard;

// Inside the window: available, labelled verified.
const fresh = card({ state: "verified", label: "verified 2026-09-20" });
equal(quorumLabel(quorumView(fresh)), "available to agents", "fresh available");
equal(evidenceSuffix(fresh, quorumView(fresh)), " · verified 2026-09-20", "fresh label");

// Past the window: still available, decayed label — never the raw
// verified_at read as "verified".
const aged = card({ state: "unverified", label: "unverified (last verified 2020-01-01)" });
equal(quorumLabel(quorumView(aged)), "available to agents", "aged still injected");
equal(
  evidenceSuffix(aged, quorumView(aged)),
  " · unverified (last verified 2020-01-01)",
  "aged label",
);

// Stale-marked: withheld with its reason, no verified label.
const marked = card({
  state: "withheld",
  label: "withheld",
  reason: "evidence marked stale: CAD-1 reverted the fix",
});
const q = quorumView(marked);
equal(quorumLabel(q), "withheld from agents", "marked withheld");
equal(q.reason, "withheld — evidence marked stale: CAD-1 reverted the fix", "marked reason");
equal(evidenceSuffix(marked, q), "", "marked has no verified label");

// A server without the evidence reading shows no label at all rather than
// the raw verified_at.
const older = card(null);
equal(evidenceSuffix(older, quorumView(older)), "", "no evidence, no label");

console.log("memory view checks passed");
