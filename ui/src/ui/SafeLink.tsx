import type { AnchorHTMLAttributes } from "react";
import { defaultUrlTransform } from "react-markdown";
import { isLoopbackHref } from "./links";

/**
 * The one place an externally-supplied href becomes an anchor (CAD-1080;
 * policy from CAD-313). Md's `a` component and every raw-href surface
 * (needs rail, overview, issue links, PR panel, milestone evidence) use it:
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
  children,
  ...rest
}: Omit<AnchorHTMLAttributes<HTMLAnchorElement>, "href" | "target" | "rel"> & {
  href: string | null | undefined;
}) {
  const safe = href ? defaultUrlTransform(href) : "";
  if (!safe) return <>{children}</>;
  if (isLoopbackHref(safe)) {
    return (
      <span
        className="text-warn"
        title="A link to this machine — not clickable on the board, so the board's session cookie is never sent to another local server. Copy it if you trust it."
      >
        {children} <code className="num break-all">[{safe}]</code>
      </span>
    );
  }
  return (
    // className first: Md's rendered anchor stays byte-identical.
    <a className={className} href={safe} target="_blank" rel="noreferrer" {...rest}>
      {children}
    </a>
  );
}
