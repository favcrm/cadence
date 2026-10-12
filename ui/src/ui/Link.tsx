import type { AnchorHTMLAttributes, MouseEvent } from "react";
import { activateNavigation, useNavigation } from "../../screen-ui/src/navigation";

export type LinkProps = AnchorHTMLAttributes<HTMLAnchorElement> & { href: string; replace?: boolean };

/**
 * An in-app link: a real `<a href>` (so it can be opened in a new tab or
 * copied) that navigates through history on a plain left click.
 */
export default function Link({
  href,
  onClick,
  replace,
  ...rest
}: LinkProps) {
  const navigation = useNavigation();
  const go = (e: MouseEvent<HTMLAnchorElement>) => {
    onClick?.(e);
    activateNavigation(navigation, href, e, { replace });
  };
  return <a href={navigation?.href(href)} onClick={go} {...rest} />;
}
