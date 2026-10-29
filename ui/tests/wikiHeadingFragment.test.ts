import type * as ReactModule from "react";
import type * as ReactDomClient from "react-dom/client";
import type * as PageTextModule from "../src/features/wiki/PageText";
import type * as LocationModule from "../src/lib/useLocation";
import type { Window as TestWindow } from "happy-dom";

declare function require<T = unknown>(name: string): T;

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

// CAD-1917 item 3: `page.md#heading` opens the target at that heading — the
// rendered heading carries a stable id and is scrolled into view, on a direct
// load, after an in-app navigation, and on a later hash change in the same page.
async function main() {
  const { Window } = require<{ Window: new (options: { url: string }) => TestWindow }>("happy-dom");
  const win = new Window({ url: "http://localhost/wiki/global/book/target.md#target-heading" });
  for (const name of ["window", "document", "navigator", "Node", "HTMLElement", "Event", "location", "history"] as const) {
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true });
  }
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true, configurable: true });
  const loader = require<{ prototype: { require: (this: unknown, id: string) => unknown } }>("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    return originalRequire.call(this, id);
  };
  const React = require<typeof ReactModule>("react");
  const { createRoot } = require<typeof ReactDomClient>("react-dom/client");
  const PageText = require<typeof PageTextModule>("../src/features/wiki/PageText").default;
  const { navigate } = require<typeof LocationModule>("../src/lib/useLocation");
  const scrolled: string[] = [];
  (win.HTMLElement.prototype as unknown as { scrollIntoView: () => void }).scrollIntoView = function (this: { id: string }) {
    scrolled.push(this.id);
  };
  const host = document.createElement("article");
  document.body.appendChild(host);
  const root = createRoot(host);
  const text = "# Target page\n\nintro\n\n## Target heading\n\nbody\n\n## Target heading\n\nagain\n\n## Other, one!\n";
  try {
    await React.act(async () => root.render(React.createElement(PageText, { path: "global/book/target.md", text })));
    const ids = [...host.querySelectorAll("h1,h2")].map((h) => h.id);
    equal(ids, ["user-content-target-page", "user-content-target-heading", "user-content-target-heading-1", "user-content-other-one"], "heading ids");
    equal(scrolled, ["user-content-target-heading"], "direct load scrolls to the heading");
    await React.act(async () => navigate("/wiki/global/book/target.md#other-one"));
    equal(scrolled[scrolled.length - 1], "user-content-other-one", "same-page navigation scrolls");
    await React.act(async () => navigate("/wiki/global/book/target.md#missing"));
    equal(scrolled.length, 2, "an unknown fragment scrolls nothing");

    // Every id is unique whatever the order; each heading is addressable by its own id.
    const idsOf = async (md: string) => {
      await React.act(async () => root.render(React.createElement(PageText, { path: "global/book/dups.md", text: md })));
      return [...host.querySelectorAll("h1,h2")].map((h) => h.id);
    };
    equal(await idsOf("# Foo\n\n## Foo\n\n## Foo-1\n"), ["user-content-foo", "user-content-foo-1", "user-content-foo-1-1"], "Foo, Foo, Foo-1");
    equal(await idsOf("## Foo-1\n\n## Foo\n\n# Foo\n"), ["user-content-foo-1", "user-content-foo", "user-content-foo-2"], "reversed");
    equal(await idsOf("## \u{1F680}\n\n## ok\n"), ["", "user-content-ok"], "an emoji-only heading gets no id");

    // No hash, an empty hash, a malformed hash or a slug-less hash never look anything up.
    const before = scrolled.length;
    for (const hash of ["", "#", "#%", "#%F0%9F%9A%80"]) {
      win.history.replaceState({}, "", "/wiki/global/book/dups.md" + hash);
      await React.act(async () => navigate("/wiki/global/book/dups.md" + hash));
      await React.act(async () => root.render(React.createElement(PageText, { path: "global/book/dups.md", text: "## \u{1F680}\n\n## Foo\n" })));
    }
    equal(scrolled.length, before, "no scroll without a usable hash");
  } finally {
    await React.act(async () => root.unmount());
    loader.prototype.require = originalRequire;
  }
}

void main().then(() => console.log("wiki heading fragment checks passed")).catch((error: unknown) => {
  console.error(error);
  (globalThis as typeof globalThis & { process: { exitCode: number } }).process.exitCode = 1;
});
