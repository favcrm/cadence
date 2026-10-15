import { clientNavHref, followClientNav, type ClientNavClick } from "../src/lib/clientNav";

function equal(actual: unknown, expected: unknown, what: string): void {
  if (actual !== expected) throw new Error(`${what}: expected ${String(expected)}, got ${String(actual)}`);
}

const ORIGIN = "http://127.0.0.1:3111";

function click(over: Partial<ClientNavClick> = {}): ClientNavClick {
  return {
    href: "/wiki/global",
    download: false,
    target: null,
    button: 0,
    metaKey: false,
    ctrlKey: false,
    shiftKey: false,
    altKey: false,
    defaultPrevented: false,
    origin: ORIGIN,
    ...over,
  };
}

/** A plain left click navigates in the app and does not let the document reload. */
function stays(href: string, what: string): void {
  const gone: string[] = [];
  let prevented = false;
  const followed = followClientNav(
    click({ href }),
    () => {
      prevented = true;
    },
    (next) => gone.push(next),
  );
  equal(followed, true, `${what} followed`);
  equal(prevented, true, `${what} does not reload`);
  equal(gone.length, 1, `${what} one navigation`);
  equal(gone[0], href, what);
}

// Tree folder, grid page, breadcrumb — the clicks that used to full-load the app.
stays("/wiki/global", "tree folder");
stays("/wiki/global/hello.md", "grid page");
stays("/wiki", "breadcrumb root");
stays("/wiki/projects/cadence/notes.md", "nested page");

equal(
  clientNavHref(click({ href: "/wiki/global/hello.md" })),
  "/wiki/global/hello.md",
  "page href",
);

/** Downloads, reserved server routes and modified clicks keep native navigation. */
function loads(over: Partial<ClientNavClick>, what: string): void {
  let prevented = false;
  const gone: string[] = [];
  const requested = click(over);
  const destination = requested.href;
  const followed = followClientNav(
    requested,
    () => {
      prevented = true;
    },
    (next) => gone.push(next),
  );
  equal(followed, false, `${what} not followed`);
  equal(prevented, false, `${what} browser proceeds`);
  equal(gone.length, 0, `${what} no navigate`);
  equal(requested.href, destination, `${what} retains its destination`);
}

loads({ href: "/api/wiki/file?path=global%2Fhello.md&raw=1", download: true }, "download");
loads({ href: "/api/wiki/file?path=global%2Fhello.md" }, "file API");
loads({ href: "/api/issues/CAD-1" }, "issues API");
loads({ href: "https://example.com/wiki/global/hello.md" }, "other origin");
loads({ href: "/wiki/global", target: "_blank" }, "new tab");
loads({ href: "/wiki/global", ctrlKey: true }, "ctrl click");
loads({ href: "/wiki/global", metaKey: true }, "meta click");
loads({ href: "/wiki/global", shiftKey: true }, "shift click");
loads({ href: "/wiki/global", button: 1 }, "middle click");
loads({ href: "/wiki/global", defaultPrevented: true }, "already handled");
loads({ href: "#section" }, "hash only");

const platformLogin = `/__platform/login?to=${encodeURIComponent(`${ORIGIN}/projects/cadence?view=list`)}`;
loads({ href: "/__platform" }, "reserved platform root");
loads({ href: platformLogin }, "hosted sign-in document handoff");
loads({ href: platformLogin, target: "_blank" }, "hosted sign-in new tab");
loads({ href: platformLogin, ctrlKey: true }, "hosted sign-in ctrl click");
loads({ href: platformLogin, metaKey: true }, "hosted sign-in meta click");
loads({ href: platformLogin, shiftKey: true }, "hosted sign-in shift click");
loads({ href: platformLogin, button: 1 }, "hosted sign-in middle click");
stays("/__platformish", "nonreserved route");

console.log("client nav checks passed");
