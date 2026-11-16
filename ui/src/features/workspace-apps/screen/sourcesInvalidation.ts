/**
 * CAD-2221 — the one host→primary "your source settings are stale" hint (see `ScreenOutlet`).
 *
 *  A confirmed source save can settle while the returned primary is still loading, can settle
 *  after the operator switched away and back, or can be batched with the scope transition so a
 *  passive effect only ever observes the final scope. Every hint therefore carries the scope it
 *  was made for and the visit (generation) of that scope at the moment the save settled:
 *
 *  - delivery requires BOTH the current scope and the current visit, so an older visit's hint
 *    can never pass even when the scope name looks unchanged (A→B→A batched, ownVisit advanced);
 *  - a hint for the current scope and visit is held until the primary is READY with a live
 *    window, then delivered once;
 *  - a refusal (no new epoch) never posts, and a new view performs its own initial source read.
 */
export interface SourcesHint { scope: string; epoch: number; visit: number }

export class SourcesInvalidator {
  private delivered = 0;
  take(input: { state: "loading" | "ready" | "fallback"; ownScope: string; ownVisit: number; hint: SourcesHint | null; liveWindow: boolean }): number | null {
    const hint = input.hint;
    if (hint === null || hint.scope !== input.ownScope || hint.visit !== input.ownVisit) return null;
    if (hint.epoch <= this.delivered) return null;
    if (input.state !== "ready" || !input.liveWindow) return null;
    this.delivered = hint.epoch;
    return hint.epoch;
  }
}
