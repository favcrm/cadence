export {};
/** CAD-2221 — the host→primary source-invalidation hint across scope visits.
 *
 *  Covers the real races: a save settling during first mounting, across an A→B→A switch, and
 *  settling after the return but batched into the same effect pass as the transition (the hint
 *  and the new visit arrive together). Old hints must never replay; a post-return save must not
 *  be suppressed; failures (no new epoch) never post.
 */
import { SourcesInvalidator } from "../src/features/workspace-apps/screen/sourcesInvalidation";

function equal(actual: unknown, expected: unknown, why: string) {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) throw new Error(`${why}: ${JSON.stringify(actual)} !== ${JSON.stringify(expected)}`);
}
const hint = (scope: string, epoch: number, visit: number) => ({ scope, epoch, visit });

// A first-mount save that settled while the frame was mounting is held, then delivered once.
const first = new SourcesInvalidator();
equal(first.take({ state: "loading", ownScope: "A", ownVisit: 0, hint: hint("A", 1, 0), liveWindow: false }), null, "first-mount save held while loading");
equal(first.take({ state: "ready", ownScope: "A", ownVisit: 0, hint: hint("A", 1, 0), liveWindow: true }), 1, "delivered when the first frame is ready");
equal(first.take({ state: "ready", ownScope: "A", ownVisit: 0, hint: hint("A", 1, 0), liveWindow: true }), null, "and not again");

// The batched race: B observed, then the return to A and a NEW hint for that visit arrive together.
const raced = new SourcesInvalidator();
equal(raced.take({ state: "ready", ownScope: "B", ownVisit: 1, hint: hint("A", 1, 0), liveWindow: true }), null, "B observed with an old A hint");
equal(raced.take({ state: "loading", ownScope: "A", ownVisit: 2, hint: hint("A", 2, 2), liveWindow: false }), null, "a post-return save batched with the transition is held");
equal(raced.take({ state: "ready", ownScope: "A", ownVisit: 2, hint: hint("A", 2, 2), liveWindow: true }), 2, "and delivered once A is ready");
equal(raced.take({ state: "ready", ownScope: "A", ownVisit: 2, hint: hint("A", 2, 2), liveWindow: true }), null, "not twice");

// The old-hint counterpart: the same transition carrying only the pre-switch hint must not replay.
const stale = new SourcesInvalidator();
equal(stale.take({ state: "ready", ownScope: "B", ownVisit: 1, hint: hint("A", 1, 1), liveWindow: true }), null, "B observed with A's old hint");
equal(stale.take({ state: "loading", ownScope: "A", ownVisit: 2, hint: hint("A", 1, 1), liveWindow: true }), null, "the old-visit A hint is dropped at the transition");
equal(stale.take({ state: "ready", ownScope: "A", ownVisit: 2, hint: hint("A", 1, 1), liveWindow: true }), null, "and never replays");
equal(stale.take({ state: "ready", ownScope: "A", ownVisit: 2, hint: hint("A", 2, 2), liveWindow: true }), 2, "a save settling after the return is still delivered");
equal(stale.take({ state: "ready", ownScope: "A", ownVisit: 2, hint: hint("A", 2, 2), liveWindow: true }), null, "and only once");

// A batched A→B→A can leave the passive effect on A while ownVisit advanced: an old visit's hint
// must never pass at the new visit, and the new visit's own hint must.
const reused = new SourcesInvalidator();
equal(reused.take({ state: "ready", ownScope: "A", ownVisit: 0, hint: hint("A", 1, 0), liveWindow: true }), 1, "visit 0 delivers its own hint");
equal(reused.take({ state: "ready", ownScope: "A", ownVisit: 2, hint: hint("A", 1, 0), liveWindow: true }), null, "a stale visit-0 hint never passes at visit 2");
equal(reused.take({ state: "ready", ownScope: "A", ownVisit: 2, hint: hint("A", 2, 2), liveWindow: true }), 2, "the current visit's hint is delivered");
equal(reused.take({ state: "ready", ownScope: "A", ownVisit: 2, hint: hint("A", 2, 2), liveWindow: true }), null, "and only once");

// Same-scope retry: no visit change, the held epoch is delivered on the retry's ready frame.
const retry = new SourcesInvalidator();
equal(retry.take({ state: "loading", ownScope: "A", ownVisit: 0, hint: { scope: "A", epoch: 1, visit: 0 }, liveWindow: false }), null, "first hint held");
equal(retry.take({ state: "fallback", ownScope: "A", ownVisit: 0, hint: hint("A", 2, 0), liveWindow: true }), null, "a failed retry holds the newest epoch");
equal(retry.take({ state: "ready", ownScope: "A", ownVisit: 0, hint: hint("A", 2, 0), liveWindow: true }), 2, "the retry's ready frame receives it once");
equal(retry.take({ state: "ready", ownScope: "A", ownVisit: 0, hint: hint("A", 2, 0), liveWindow: true }), null, "no duplicate");
equal(retry.take({ state: "ready", ownScope: "A", ownVisit: 0, hint: hint("A", 2, 0), liveWindow: false }), null, "a lost window does not repost a delivered hint");

console.log("sourcesInvalidation ok");
