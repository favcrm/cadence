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

/** The scroller itself resized (window height, a notice mounting above
 *  it): re-pin only when the reader was on the tail BEFORE the change —
 *  a measurement after a shrink is already short of the tail. */
export function repinOnResize(
  wasOnTail: boolean,
  prevClientHeight: number,
  clientHeight: number,
): boolean {
  return wasOnTail && prevClientHeight !== clientHeight;
}

/** The jump pill's words: the count rides along while there is one.
 *  `t` translates each phrase (default: English), so the two surfaces
 *  that show this pill — Home and the app-pane chat — share one wording
 *  and neither is English-only in a translated locale (CAD-1533). */
export function jumpLabel(unseen: number, t: (english: string) => string = (english) => english): string {
  const base = t("Jump to latest");
  return unseen > 0 ? `${base} · ${unseen} ${t("new")}` : base;
}
