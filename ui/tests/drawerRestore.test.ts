export {};
/** CAD-2221 — the one-shot drawer restore behind a connect return.
 *
 *  The regression this guards: the first mount of the returned app screen can fail (a refused
 *  or timed-out mount). The hint must survive that failure so the operator's Retry, once the
 *  frame is ready, still opens the drawer the connect started from — exactly once. A hint is
 *  never consumed while the screen is loading or after a failed mount.
 */
import { DrawerRestorer } from "../src/features/workspace-apps/screen/drawerRestore";

function equal(actual: unknown, expected: unknown, why: string) {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) throw new Error(`${why}: ${JSON.stringify(actual)} !== ${JSON.stringify(expected)}`);
}
function assert(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }

const hint = { surface: "settings", size: "wide" } as const;
const restore = new DrawerRestorer("install:digest:tag:context");

equal(restore.take("loading", hint), null, "a loading frame opens nothing and keeps the hint");
equal(restore.take("fallback", hint), null, "a failed first mount must not consume the hint");
equal(restore.take("ready", null), null, "a ready frame with no hint opens nothing");
equal(restore.take("ready", hint), hint, "the retry's ready frame restores the surface");
equal(restore.take("ready", hint), null, "a second ready does not reopen it");
equal(restore.take("loading", hint), null, "a later reload of the same identity does not reopen it");

// Another frame identity (install/context/digest/tag) does not inherit the latch.
const other = new DrawerRestorer("install-2:digest:tag:context");
equal(other.take("ready", hint), hint, "a different identity restores its own hint");

// The size the drawer was opened with travels with the restore, and a size-less hint stays valid.
equal(new DrawerRestorer("s").take("ready", { surface: "settings" }), { surface: "settings" }, "a size-less hint restores the default width");
assert(new DrawerRestorer("s").take("fallback", hint) === null, "failure never reports a drawer to open");

console.log("drawerRestore ok");
