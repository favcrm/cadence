import type { ActionRefusal, ActionResult, ReplyMessage, SlotAnchor, SlotRequest, SlotVerb } from "./screenProtocol";

/**
 * CAD-1123 HP3 — the host-drawn approval slot (decision Q6). A frame may ask
 * for a spend or publish verb, but only a tap on a button the HOST draws, in
 * the parent page over the iframe, runs it. This controller owns the rules,
 * apart from any DOM so they can be tested:
 *
 *  - at most one live slot per frame: a new request (a different id, or the
 *    same id with different args) retires the old slot as `superseded`;
 *  - a slot's label and the work it runs come from `planner` (host data),
 *    never from the frame; the frame never sees a token or an approval id;
 *  - taps are ignored for `SLOT_GUARD_MS` after the slot appears and after it
 *    moves (a re-sent anchor that differs), measured from BOTH the press and
 *    the click, so a slot that slides under a cursor cannot fire;
 *  - only a trusted (user-agent generated) press counts; a tap runs once, a
 *    second tap while it is running does nothing;
 *  - the slot dies on close (remount, scope change) and after `SLOT_TTL_MS`.
 */
export const SLOT_GUARD_MS = 500;
export const SLOT_TTL_MS = 120_000;

/** CAD-1328: what a confirm card shows, from the daemon's frozen effect material (never from the frame). */
export interface SlotCard { target: string; caption: string; image: string | null }
export interface SlotPlan { label: string; run: () => Promise<ActionResult>; card?: SlotCard }
export type Planner = (verb: SlotVerb, args: Record<string, unknown>, ui?: { actionToken?: string }) => Promise<SlotPlan | ActionRefusal>;
export interface SlotView { token: string; label: string; anchor: SlotAnchor; pending: boolean; card?: SlotCard }

interface Live { id: string; key: string; view: SlotView; since: number; plan: SlotPlan; busy: boolean; timer: ReturnType<typeof setTimeout> }

const sameAnchor = (a: SlotAnchor, b: SlotAnchor) => a === b ||
  (typeof a === "object" && typeof b === "object" && a.x === b.x && a.y === b.y && a.w === b.w && a.h === b.h);
const randomToken = () => Array.from(crypto.getRandomValues(new Uint8Array(16)), b => b.toString(16).padStart(2, "0")).join("");
const UNEXPECTED: ActionRefusal = { code: "failed", text: "That didn't work. Nothing was started." };

export class SlotController {
  private live: Live | null = null;
  private planning: { id: string; key: string; anchor: SlotAnchor } | null = null;
  private seq = 0;
  private closed = false;
  constructor(private send: (message: ReplyMessage) => void, private changed: (view: SlotView | null) => void,
    private planner: Planner, private clock: () => number = () => performance.now()) {}

  /** The view the host draws, or `null` when there is no live slot. */
  view(): SlotView | null { return this.live?.view ?? null; }

  async request(req: SlotRequest): Promise<void> {
    if (this.closed) return;
    const key = JSON.stringify([req.verb, req.args]);
    if (this.live?.busy) { this.send({ v: 2, op: "slot-state", id: req.id, state: "refused", refusal: { code: "busy", text: "Finish the current action first." } }); return; }
    if (this.live?.id === req.id && this.live.key === key) {
      if (!sameAnchor(this.live.view.anchor, req.anchor)) {
        this.live.view = { ...this.live.view, anchor: req.anchor };
        this.live.since = this.clock();
        this.changed(this.live.view);
      }
      return;
    }
    if (this.planning?.id === req.id && this.planning.key === key) { this.planning.anchor = req.anchor; return; }
    this.retire("superseded");
    const mine = ++this.seq;
    this.planning = { id: req.id, key, anchor: req.anchor };
    let plan: SlotPlan | ActionRefusal;
    try { plan = await this.planner(req.verb, req.args); } catch { plan = UNEXPECTED; }
    if (this.closed || mine !== this.seq) return;
    const anchor = this.planning?.anchor ?? req.anchor;
    this.planning = null;
    if (!("run" in plan)) { this.send({ v: 2, op: "slot-state", id: req.id, state: "refused", refusal: plan }); return; }
    const timer = setTimeout(() => { if (this.live?.id === req.id && !this.live.busy) this.retire("expired"); }, SLOT_TTL_MS);
    this.live = { id: req.id, key, view: { token: randomToken(), label: plan.label, anchor, pending: false, ...(plan.card ? { card: plan.card } : {}) }, since: this.clock(), plan, busy: false, timer };
    this.changed(this.live.view);
  }

  /** A press on the host button. `pressedAt` is the `pointerdown` time on the
   *  same clock (or now, for keyboard activation). Returns whether it fired. */
  tap(token: string, trusted: boolean, pressedAt: number): boolean {
    const live = this.live;
    if (this.closed || !live || live.busy || !trusted || token !== live.view.token) return false;
    const now = this.clock();
    if (now - live.since < SLOT_GUARD_MS || pressedAt - live.since < SLOT_GUARD_MS) return false;
    live.busy = true;
    live.view = { ...live.view, pending: true };
    this.send({ v: 2, op: "slot-state", id: live.id, state: "pending" });
    this.changed(live.view);
    void live.plan.run().catch((): ActionResult => ({ ok: false, refusal: UNEXPECTED })).then(result => {
      if (this.closed || this.live !== live) return;
      clearTimeout(live.timer);
      this.live = null;
      this.send(result.ok ? { v: 2, op: "slot-state", id: live.id, state: "done" }
        : { v: 2, op: "slot-state", id: live.id, state: "refused", refusal: result.refusal });
      this.changed(null);
    });
    return true;
  }

  /** The person closed a confirm card: nothing runs, the frame is told. */
  cancel(token: string): void {
    if (this.live && !this.live.busy && token === this.live.view.token) this.retire("cancelled");
  }

  private retire(code: string): void {
    const live = this.live;
    if (!live) return;
    clearTimeout(live.timer);
    this.live = null;
    this.send({ v: 2, op: "slot-state", id: live.id, state: "refused", refusal: { code, text: code === "expired" ? "That took too long. Try again." : code === "cancelled" ? "Cancelled. Nothing was posted." : "Replaced by another action." } });
    this.changed(null);
  }

  close(): void {
    this.closed = true;
    this.seq++;
    if (this.live) clearTimeout(this.live.timer);
    this.live = null;
    this.planning = null;
    this.changed(null);
  }
}

/** The host button's box inside the frame area `width` x `height`: the
 *  frame's requested rectangle, clamped fully inside the frame and never
 *  smaller than a usable target. `footer` is the host bar along the bottom. */
export function slotBox(anchor: SlotAnchor, width: number, height: number):
  { x: number; y: number; w: number; h: number; footer: boolean } {
  if (anchor === "footer") {
    const h = Math.min(56, height);
    return { x: 0, y: Math.max(0, height - h), w: width, h, footer: true };
  }
  const w = Math.min(Math.max(anchor.w, 44), width);
  const h = Math.min(Math.max(anchor.h, 32), height);
  return { x: Math.min(Math.max(anchor.x, 0), Math.max(0, width - w)),
    y: Math.min(Math.max(anchor.y, 0), Math.max(0, height - h)), w, h, footer: false };
}
