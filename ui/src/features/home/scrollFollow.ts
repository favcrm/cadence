/** CAD-610: sending in the master chat pins the thread to its tail.
 *  CAD-551 smart scroll still stands down when the operator scrolls up. */

export const TAIL_SLACK = 180;

export interface Follow {
  /** Follow new rows (the working indicator, tool steps, the reply). */
  pinned: boolean;
  /** A send or pill glide is in flight; position events must not unpin it. */
  programmatic: boolean;
  /** Rows that arrived while the operator was reading history. */
  unseen: number;
}

export type ScrollKind = "glide" | "jump";

export function initialFollow(): Follow {
  return { pinned: true, programmatic: false, unseen: 0 };
}

/** The tail's document Y sits inside the viewport, within `slack` px.
 *  The working row is the last block in the thread section, so a near
 *  tail means that indicator is on screen. */
export function nearTail(
  tail: number,
  scrollY: number,
  viewportHeight: number,
  slack = TAIL_SLACK,
): boolean {
  return tail <= viewportHeight + scrollY + slack && tail >= scrollY - slack;
}

/** `scrollY` that places `tail` on the viewport's bottom edge. */
export function tailScrollTop(tail: number, viewportHeight: number): number {
  return Math.max(0, tail - viewportHeight);
}

/** Glides are smooth; a reduced-motion preference, and every follow-up
 *  while a turn streams, jumps. */
export function motionBehavior(reduced: boolean, kind: ScrollKind): ScrollBehavior {
  if (kind === "jump" || reduced) return "auto";
  return "smooth";
}

/** Enter or the send button — a chat line, a slash command, or an
 *  Ask-master draft. Pins even when the operator was reading history. */
export function onSend(_state: Follow): { state: Follow; scroll: ScrollKind } {
  return {
    state: { pinned: true, programmatic: true, unseen: 0 },
    scroll: "glide",
  };
}

/** The "↓ new messages" pill is the same pin as a send. */
export function onPill(state: Follow): { state: Follow; scroll: ScrollKind } {
  return onSend(state);
}

/** A scroll event. An in-flight glide does not unpin; arriving at the
 *  tail releases the lock. Otherwise the position is the pin. */
export function onViewportScroll(state: Follow, near: boolean): Follow {
  if (state.programmatic) {
    if (!near) return state;
    return { pinned: true, programmatic: false, unseen: 0 };
  }
  return {
    pinned: near,
    programmatic: false,
    unseen: near ? 0 : state.unseen,
  };
}

/** The glide's `scrollend`. If the tail grew past the target while the
 *  turn was working, one jump catches the indicator. */
export function onScrollSettled(
  state: Follow,
  near: boolean,
): { state: Follow; scroll: ScrollKind | "none" } {
  if (!state.programmatic) return { state, scroll: "none" };
  if (near || !state.pinned) {
    return {
      state: {
        pinned: state.pinned && near,
        programmatic: false,
        unseen: near ? 0 : state.unseen,
      },
      scroll: "none",
    };
  }
  return { state: { ...state, programmatic: false }, scroll: "jump" };
}

/** Wheel, touch, or a key that moves the page upward. Drops the glide
 *  lock so the rest of the turn does not yank the view back down. */
export function onOperatorScrollUp(state: Follow): Follow {
  if (!state.pinned && !state.programmatic) return state;
  return { pinned: false, programmatic: false, unseen: state.unseen };
}

/** The thread tail moved (a pending send, a tool step, a reply).
 *  Pinned follows with a jump, unless the send glide already owns this
 *  frame. Unpinned rows count onto the pill. */
export function onTailChange(
  state: Follow,
  delta: number,
): { state: Follow; scroll: ScrollKind | "none" } {
  if (!state.pinned) {
    if (delta > 0) {
      return { state: { ...state, unseen: state.unseen + delta }, scroll: "none" };
    }
    return { state, scroll: "none" };
  }
  if (state.programmatic) return { state, scroll: "none" };
  return { state, scroll: "jump" };
}
