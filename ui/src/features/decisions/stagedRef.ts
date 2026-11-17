/**
 * CAD-2364: which staged write, if any, an identifier in an assistant answer
 * names. Only the rows the viewer's own decisions list already holds are
 * searched, so an id the board does not show this viewer matches nothing and
 * stays a plain token. No React, so the UI tests load it directly.
 */
import { isolate, outcomeOf, stagedDomain, stagedDomainLabel, type StagedOutcome, type StagedWrite } from "./stagedWrite";

/** A prefix shorter than this never stands for an id. */
export const MIN_PREFIX = 8;

const norm = (id: string): string => id.toLowerCase().replace(/-/g, "");

/**
 * The comparable form of an identifier as an answer writes it, in full or
 * cut short (`828c1018…`, `828c1018...`): lowercase, no dashes, no trailing
 * ellipsis. Null when it is not shaped like an id or is too short to be one.
 */
export function stagedRefKey(token: string): string | null {
  const bare = token.trim().replace(/(?:…|\.\.\.)$/, "");
  if (!/^[0-9a-z_-]+$/i.test(bare)) return null;
  const key = norm(bare);
  return key.length >= MIN_PREFIX ? key : null;
}

/**
 * The one write `token` names: its full id, else the only id it is a prefix
 * of. No match, or a prefix shared by two or more writes, is null.
 */
export function matchStaged(token: string, rows: readonly StagedWrite[]): StagedWrite | null {
  const key = stagedRefKey(token);
  if (key === null) return null;
  const exact = rows.filter((r) => norm(r.id) === key);
  if (exact.length > 0) return exact.length === 1 ? exact[0] : null;
  const hits = rows.filter((r) => norm(r.id).startsWith(key));
  return hits.length === 1 ? hits[0] : null;
}

/** The rail's own words for each outcome (English catalogue keys). */
const STATUS: Readonly<Record<StagedOutcome, string>> = {
  pending: "Waiting",
  applied: "Applied",
  denied: "Denied",
  failed: "Did not apply",
  unconfirmed: "Result not confirmed",
};

export interface StagedChipView {
  title: string;
  app: string | null;
  /** English catalogue key, the review card's domain pill. */
  kind: string;
  /** English catalogue key. */
  status: string;
  outcome: StagedOutcome;
  /** Only a waiting write has a review card to open. */
  pending: boolean;
}

/** What a chip shows for one write, read from the row as it is now. */
export function stagedChipView(write: StagedWrite): StagedChipView {
  const outcome = outcomeOf(write);
  return {
    title: isolate(write.title),
    app: write.app === null ? null : isolate(write.app),
    kind: stagedDomainLabel(stagedDomain(write)),
    status: STATUS[outcome],
    outcome,
    pending: outcome === "pending",
  };
}
