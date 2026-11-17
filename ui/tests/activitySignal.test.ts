/**
 * One user gesture marks exactly one request; presence alone marks none.
 */
import type * as HappyDom from "happy-dom";
declare function require(name: string): unknown;
declare const process: { exit(code: number): never };
export {};

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
  const { Window } = require("happy-dom") as typeof HappyDom;
  const win = new Window({ url: "http://localhost/" });
  for (const name of ["window", "document", "Event", "MouseEvent", "KeyboardEvent", "localStorage"] as const)
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  const loader = require("module") as { prototype: { require: (this: unknown, id: string) => unknown } };
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    return originalRequire.apply(this, [id]);
  };

  const { activityHeaders } = require("../src/lib/activity") as {
    activityHeaders(): Record<string, string>;
  };
  const { hostedPresence, markHostedPresence, clearHostedPresence } =
    require("../src/features/auth/presence") as {
      hostedPresence(): boolean;
      markHostedPresence(): void;
      clearHostedPresence(): void;
    };

  assert(Object.keys(activityHeaders()).length === 0, "a timer carries no activity");
  win.dispatchEvent(new win.MouseEvent("pointerdown"));
  assert(activityHeaders()["X-Cadence-Activity"] === "user", "a gesture marks its request");
  assert(Object.keys(activityHeaders()).length === 0, "one gesture marks one request");

  const realNow = Date.now;
  win.dispatchEvent(new win.KeyboardEvent("keydown"));
  try {
    Date.now = () => realNow() + 5_000;
    assert(Object.keys(activityHeaders()).length === 0, "a stale mark expires");
  } finally {
    Date.now = realNow;
  }
  assert(activityHeaders()["X-Cadence-Activity"] === "user", "a fresh gesture marks again");

  assert(!hostedPresence(), "no presence before a sign-in");
  markHostedPresence();
  assert(hostedPresence(), "presence survives for the returning browser");
  clearHostedPresence();
  assert(!hostedPresence(), "sign-out clears presence");
  console.log("activity and presence checks passed");
}

void main().catch((e: unknown) => { console.error(e); process.exit(1); });
