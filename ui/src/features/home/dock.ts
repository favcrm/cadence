/**
 * CAD-600: the Home conversation panel's dock and scroll math. Pure —
 * Home measures the panel and the dock; these decide when the thread
 * follows its tail, how far "the bottom" is, and what the jump pill
 * says. Unit-tested in plain node (tests/homeDock.test.ts).
 */

/** The dock's measured height as a CSS custom property (Home sets it):
 *  the thread's bottom padding and the pill's floor are both
 *  `calc(var(--dock-h) + …)`, so the last message is never hidden. */
export const DOCK_VAR = "--dock-h";

/** How close to the tail still counts as reading the tail, in px. */
export const TAIL_SLACK = 48;

/** Is the scroller within `slack` px of its bottom? */
export function atTail(
  scrollTop: number,
  scrollHeight: number,
  clientHeight: number,
  slack = TAIL_SLACK,
): boolean {
  return scrollHeight - scrollTop - clientHeight <= slack;
}

/** The scrollTop that shows the tail — never negative (a scroller whose
 *  content fits is already at its bottom). */
export function tailTop(scrollHeight: number, clientHeight: number): number {
  return Math.max(0, scrollHeight - clientHeight);
}

/** The jump pill's words: the count rides along while there is one. */
export function jumpLabel(unseen: number): string {
  return unseen > 0 ? `Jump to latest · ${unseen} new` : "Jump to latest";
}
