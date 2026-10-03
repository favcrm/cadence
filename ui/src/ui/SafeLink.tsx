import type { AnchorHTMLAttributes } from "react";
import { defaultUrlTransform } from "react-markdown";
import { isLoopbackHref } from "./links";

/**
 * The one place an externally-supplied href becomes an anchor (CAD-1080;
 * policy from CAD-313). Md's `a` component and every raw-href surface
 * (needs rail, overview, issue links, PR panel, issue page, lane card,
 * milestone evidence) use it:
 *  - an unsafe scheme (`javascript:` and friends, by react-markdown's own
 *    transform) or an empty href renders the children as plain text;
 *  - a link to THIS machine renders a warning, never an anchor, so the
 *    board's session cookie is never sent to another local listener;
 *  - anything else is an external anchor in a new tab.
 * Purely internal router links keep using Link.
 */
export default function SafeLink({
  href,
  className,
  role,
  warnClassName,
  children,
  ...rest
}: Omit<AnchorHTMLAttributes<HTMLAnchorElement>, "href" | "target" | "rel"> & {
  href: string | null | undefined;
  /** Extra classes for the loopback warning span (e.g. a menu item's). */
  warnClassName?: string;
}) {
  const safe = href ? defaultUrlTransform(href) : "";
  // A menu item keeps its role when it is not an anchor; Md passes none.
  if (!safe) return role ? <span role={role} className={warnClassName}>{children}</span> : <>{children}</>;
  if (isLoopbackHref(safe)) {
    return (
      <span
        role={role}
        className={warnClassName ? `text-warn ${warnClassName}` : "text-warn"}
        title="A link to this machine — not clickable on the board, so the board's session cookie is never sent to another local server. Copy it if you trust it."
      >
        {children} <code className="num break-all">[{safe}]</code>
      </span>
    );
  }
  return (
    // className first: Md's rendered anchor stays byte-identical.
    <a className={className} href={safe} target="_blank" rel="noreferrer" role={role} {...rest}>
      {children}
    </a>
  );
}
