/**
 * CAD-1615 — the board-wide drawer for an app screen: the app asks for it
 * with its own verb, the host owns the geometry, and the reply names the
 * surface the app is now in.
 */
import type { runCall } from "../src/features/workspace-apps/screen/screenActions";
import { CALL_VERBS } from "../src/features/workspace-apps/screen/screenProtocol";

declare function require(name: string): any;
export {};
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
function equal(a: unknown, e: unknown, why: string) {
  if (JSON.stringify(a) !== JSON.stringify(e)) throw new Error(`${why}: expected ${JSON.stringify(e)}, got ${JSON.stringify(a)}`);
}

type ActionContext = Parameters<typeof runCall>[0];
type ActionUi = Parameters<typeof runCall>[3];

async function main() {
  const { runCall: call } = require("../src/features/workspace-apps/screen/screenActions") as { runCall: typeof runCall };

  // The drawer verbs read no installation field, so the fixture names only
  // what the call path touches.
  const installation = { install_id: "install-1", name: "social-content", title: "Social Content" } as ActionContext["installation"];
  const ctx: ActionContext = { installation, installId: "install-1", contextId: "ctx-1", runs: [], onChanged: () => {} };

  const opened: string[] = [];
  const closed: string[] = [];
  const ui: ActionUi = {
    showLink: () => {},
    openScreenDrawer: (surface: string) => { opened.push(surface); return true; },
    closeScreenDrawer: () => { closed.push("outlet"); return true; },
  };

  // The verbs are part of the closed set a frame may call.
  assert((CALL_VERBS as readonly string[]).includes("screen.drawer.open"), "the drawer verb is in the closed call set");
  assert((CALL_VERBS as readonly string[]).includes("screen.drawer.close"), "the drawer close verb is in the closed call set");

  const open = await call(ctx, "screen.drawer.open", { surface: "settings" }, ui);
  assert(open.ok, "the drawer request is granted");
  equal(open.ok ? open.data : null, { mode: "drawer", surface: "settings" }, "the reply names the surface");
  equal(opened, ["settings"], "the host was asked to present the surface");

  const close = await call(ctx, "screen.drawer.close", {}, ui);
  assert(close.ok, "the drawer close is granted");
  equal(close.ok ? close.data : null, { mode: "outlet" }, "the reply names the outlet");
  equal(closed, ["outlet"], "the host was asked to put the screen back");

  // Without a shell behind the frame there is no drawer to open.
  const noShell = await call(ctx, "screen.drawer.open", { surface: "settings" }, { showLink: () => {} });
  assert(!noShell.ok, "a frame with no shell cannot open a drawer");
console.log("screen drawer checks passed");
}
void main();
