/**
 * Hand focus back after the shared work-item detail closes (CAD-1811).
 *
 * DrawerShell restores the opener on Escape and backdrop close. A Home
 * decision's "Not now" and a finished reading queue unmount the detail
 * directly, so the host calls this too. The opener is the control that
 * opened the detail while it is still in the document — never a node from
 * inside the removed drawer — else the board's heading takes focus, so a
 * keyboard user never lands on `<body>`.
 */
export function restoreDetailFocus(opener: Element | null): void {
  const run = () => {
    // An action may have moved focus somewhere deliberate (the Home composer
    // after "Change something…"): never overwrite a live control outside the
    // close. Only an unclaimed focus (body) or the removed drawer is ours.
    const active = document.activeElement;
    const claimed =
      active instanceof HTMLElement &&
      active !== document.body &&
      active.isConnected &&
      !active.closest("[data-drawer]");
    if (claimed) return;
    const back =
      opener instanceof HTMLElement && opener !== document.body && opener.isConnected && !opener.closest("[data-drawer]")
        ? opener
        : null;
    if (back !== null) {
      back.focus({ preventScroll: true });
      return;
    }
    const heading = document.querySelector<HTMLElement>("[data-outlet-heading]");
    if (heading === null) return;
    if (!heading.hasAttribute("tabindex")) heading.tabIndex = -1;
    heading.focus({ preventScroll: true });
  };
  // Browsers schedule after the removed drawer's layout and after any
  // explicit focus request from the closing action; a frame-less host (the
  // test DOM) still needs the restore to run.
  if (typeof requestAnimationFrame === "function") requestAnimationFrame(() => window.setTimeout(run, 0));
  else window.setTimeout(run, 0);
}
