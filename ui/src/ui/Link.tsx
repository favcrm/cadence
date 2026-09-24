import type { AnchorHTMLAttributes, MouseEvent } from "react";
import { navigate } from "../lib/useLocation";

/**
 * An in-app link: a real `<a href>` (so it can be opened in a new tab or
 * copied) that navigates through history on a plain left click.
 */
export default function Link({
  href,
  onClick,
  replace,
  ...rest
}: AnchorHTMLAttributes<HTMLAnchorElement> & { href: string; replace?: boolean }) {
  const go = (e: MouseEvent<HTMLAnchorElement>) => {
    onClick?.(e);
    if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) {
      return;
    }
    e.preventDefault();
    navigate(href, { replace });
  };
  return <a href={href} onClick={go} {...rest} />;
}
