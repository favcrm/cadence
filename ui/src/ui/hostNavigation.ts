import type { NavigationAdapter } from "../../screen-ui/src/navigation";
import { navigate } from "../lib/useLocation";

/** Board-only authority adapter. Never exported by @cadence/screen-ui. */
export const hostNavigation: NavigationAdapter = {
  navigate,
  href: (href) => href,
  modifiedClicks: "browser",
};
