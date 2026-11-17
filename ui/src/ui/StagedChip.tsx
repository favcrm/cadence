import { createContext, useSyncExternalStore, type ReactNode } from "react";
import { useTranslate } from "../lib/localeText";
import { matchStaged, stagedChipView } from "../features/decisions/stagedRef";
import type { DecisionsStore } from "../features/decisions/useDecisions";

/** The decisions a chat surface already reads for its own review card. */
export type StagedSource = Pick<DecisionsStore, "subscribe" | "currentDecisions" | "requestReview">;

/**
 * CAD-2364: set by a surface that shows staged-write review cards (Home with
 * its rail, an app pane with its conversation's decisions). Without one, an
 * answer's identifiers stay plain tokens.
 */
export const StagedSourceContext = createContext<StagedSource | null>(null);

/**
 * An identifier in an answer that may name a staged write. It reads the list
 * the surface already holds (never a fetch of its own), so the status follows
 * the review card as it changes. An id with no single match is `fallback`.
 * The chip only opens the card: approving and denying stay on the card.
 */
export default function StagedChip({ source, token, fallback }: { source: StagedSource; token: string; fallback: ReactNode }) {
  const t = useTranslate();
  const { rows } = useSyncExternalStore(source.subscribe, source.currentDecisions, source.currentDecisions);
  const write = matchStaged(token, rows);
  if (write === null) return <>{fallback}</>;
  const view = stagedChipView(write);
  const meta = [view.app, t(view.kind)].filter((part): part is string => !!part).join(" · ");
  const status = t(view.status);
  return (
    <button
      type="button"
      className="stagedchip"
      data-staged-chip
      data-outcome={view.outcome}
      aria-disabled={view.pending ? undefined : true}
      aria-label={`${view.title} · ${meta} · ${status}`}
      title={t(view.pending ? "Open its review card" : "Decided on the board")}
      onClick={(event) => {
        if (view.pending) source.requestReview(write.id, event.currentTarget);
      }}
    >
      <span className="stagedchip-title"><bdi>{view.title}</bdi></span>
      <span className="stagedchip-meta"><bdi>{meta}</bdi></span>
      <span className="stagedchip-status">{status}</span>
    </button>
  );
}
