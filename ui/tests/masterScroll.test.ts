/**
 * CAD-610: sending while scrolled up pins the master thread to its tail
 * so the working indicator is in view. Scrolling up during the turn
 * stops the pin and the next row shows the "↓ new message" pill.
 * `prefers-reduced-motion` jumps instead of gliding.
 */
import {
  initialFollow,
  motionBehavior,
  nearTail,
  onOperatorScrollUp,
  onPill,
  onScrollSettled,
  onSend,
  onTailChange,
  onViewportScroll,
  tailScrollTop,
  type Follow,
} from "../src/features/home/scrollFollow";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

function ok(cond: boolean, what: string): void {
  if (!cond) throw new Error(`failed: ${what}`);
}

// The working row sits at the thread tail. Scrolled up, that tail is
// far below the viewport.
const tail = 4000;
const vh = 800;
const readingHistory = 200;
ok(!nearTail(tail, readingHistory, vh), "scrolled up: working indicator is off screen");

let follow: Follow = { ...initialFollow(), pinned: false };
const sent = onSend(follow);
follow = sent.state;
equal(sent.scroll, "glide", "send glides to the tail");
ok(follow.pinned && follow.unseen === 0 && follow.programmatic, "send pins and clears the pill");

// Position events while the glide is in flight must not unpin.
follow = onViewportScroll(follow, false);
ok(follow.pinned && follow.programmatic, "mid-glide scroll stays pinned");

const top = tailScrollTop(tail, vh);
ok(nearTail(tail, top, vh), "after the glide the working indicator is in view");
equal(top, 3200, "scrollY places the tail on the viewport bottom");
follow = onViewportScroll(follow, true);
ok(follow.pinned && !follow.programmatic, "arrival releases the glide lock");

// A tool step lands while the turn is working and the view is pinned.
let step = onTailChange(follow, 1);
ok(step.scroll === "jump" && step.state.unseen === 0 && step.state.pinned, "pinned turn follows the step");

// The operator scrolls up during the turn: stop pinning.
follow = onViewportScroll(step.state, false);
ok(!follow.pinned && !follow.programmatic, "scroll up stops auto-pinning");

// The reply arrives off screen and counts onto the pill.
const reply = onTailChange(follow, 1);
ok(reply.scroll === "none" && reply.state.unseen === 1, "pill shows the one new message");
ok(!reply.state.pinned, "the reply does not yank the view back");

// The same gesture, issued while a glide is still in flight, cancels it.
const cancelled = onOperatorScrollUp({ pinned: true, programmatic: true, unseen: 0 });
ok(!cancelled.pinned && !cancelled.programmatic, "scroll-up gesture cancels the glide");
const during = onTailChange(cancelled, 1);
ok(during.state.unseen === 1 && during.scroll === "none", "a cancelled glide counts the next row on the pill");

// Slash commands and Ask-master drafts share onSend — a second send
// from the pill's scrolled-up state pins again.
const again = onSend(during.state);
ok(again.state.pinned && again.state.unseen === 0 && again.scroll === "glide", "another send re-pins");

// The pill button is the same pin.
const pill = onPill({ pinned: false, programmatic: false, unseen: 2 });
ok(pill.state.pinned && pill.state.unseen === 0 && pill.scroll === "glide", "pill jump re-pins");

// Tail grew during the glide: settle jumps once so the indicator stays in view.
const grew = onScrollSettled({ pinned: true, programmatic: true, unseen: 0 }, false);
equal(grew.scroll, "jump", "a tail that outgrew the glide jumps");
ok(grew.state.pinned && !grew.state.programmatic, "the jump stays pinned without a lock");

// Settling on the tail just releases the lock.
const landed = onScrollSettled({ pinned: true, programmatic: true, unseen: 0 }, true);
equal(landed.scroll, "none", "landing on the tail does not scroll again");
ok(landed.state.pinned && !landed.state.programmatic, "landing clears the lock");

// An idle scroll-end changes nothing.
const idle = onScrollSettled({ pinned: false, programmatic: false, unseen: 1 }, true);
equal(idle.state.unseen, 1, "idle settle keeps the pill count");
equal(idle.scroll, "none", "idle settle does not scroll");

// Already unpinned: the gesture is a no-op (same state).
const parked = { pinned: false, programmatic: false, unseen: 2 };
ok(onOperatorScrollUp(parked) === parked, "scroll-up while parked is a no-op");

// Reduced motion jumps; a streaming follow-up always jumps.
equal(motionBehavior(true, "glide"), "auto", "reduced motion jumps on send");
equal(motionBehavior(false, "glide"), "smooth", "send glides when motion is allowed");
equal(motionBehavior(false, "jump"), "auto", "a follow-up jump is instant");

// No new rows while unpinned does not bump the pill.
const quiet = onTailChange({ pinned: false, programmatic: false, unseen: 1 }, 0);
equal(quiet.state.unseen, 1, "a zero delta leaves the pill");
equal(quiet.scroll, "none", "a zero delta does not scroll");

console.log("masterScroll.test.ts: all assertions passed");
