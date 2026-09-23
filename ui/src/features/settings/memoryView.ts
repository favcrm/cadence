import type { MemoryCard } from "../../lib/types";

export type QuorumView = {
  eligible: boolean | null;
  reason: string;
  mode: "acceptance" | "retrieval";
  /** Retrieval-eligible but withheld: its evidence is marked stale. */
  withheld?: boolean;
};

export function quorumView(m: MemoryCard): QuorumView {
  const mode = m.status === "proposed" ? "acceptance" : "retrieval";
  if (!m.quorum) {
    return {
      eligible: null,
      reason: "verification unavailable — this server did not report quorum",
      mode,
    };
  }

  const check = mode === "acceptance" ? m.quorum.accept : m.quorum;
  if (!check || typeof check.eligible !== "boolean") {
    return {
      eligible: null,
      reason: "verification unavailable — this server did not report quorum",
      mode,
    };
  }

  // The same Freshness retrieval uses: a stale-marked lesson passes its
  // quorum yet is never injected — show it withheld, with its reason.
  if (mode === "retrieval" && check.eligible && m.evidence?.state === "withheld") {
    return {
      eligible: false,
      reason: `withheld — ${m.evidence.reason || "evidence marked stale"}`,
      mode,
      withheld: true,
    };
  }

  return {
    eligible: check.eligible,
    reason: check.reason || "server did not provide a quorum reason",
    mode,
  };
}

export function quorumTone(eligible: boolean | null): string {
  if (eligible === true) return "bg-accent/10 text-accent";
  if (eligible === false) return "bg-warn/10 text-warn";
  return "bg-ink-800 text-ink-400";
}

export function quorumLabel(q: QuorumView): string {
  if (q.eligible === null) return "verification unavailable";
  if (q.mode === "acceptance") {
    return q.eligible ? "awaiting PM finalization" : "review blocked";
  }
  if (q.withheld) return "withheld from agents";
  return q.eligible ? "available to agents" : "not available to agents";
}

/** The evidence label an available lesson is injected with — the
 * server's retrieval reading ("verified <date>" only inside the window,
 * else "unverified (last verified <date>)"), never the raw verified_at. */
export function evidenceSuffix(m: MemoryCard, q: QuorumView): string {
  if (q.mode !== "retrieval" || q.eligible !== true || !m.evidence?.label) return "";
  return ` · ${m.evidence.label}`;
}
