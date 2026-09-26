/**
 * Plain left-clicks on an href Button follow the in-app router (CAD-609).
 * Modified clicks keep the browser's own behaviour so a new tab still works.
 */
export type ButtonClickLike = {
  defaultPrevented: boolean;
  button: number;
  metaKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  preventDefault(): void;
};

export function activateButtonHref(
  href: string,
  event: ButtonClickLike,
  navigateFn: (href: string, opts?: { replace?: boolean }) => void,
  opts?: { replace?: boolean },
): void {
  if (
    event.defaultPrevented ||
    event.button !== 0 ||
    event.metaKey ||
    event.ctrlKey ||
    event.shiftKey ||
    event.altKey
  ) {
    return;
  }
  event.preventDefault();
  navigateFn(href, opts);
}
