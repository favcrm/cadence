import type { MilestoneRow } from "../src/lib/types";

declare function require(name: string): any;
// The icon packages are ESM-only; stub them the way the other view tests do.
const moduleLoader = require("module");
const originalRequire = moduleLoader.prototype.require;
moduleLoader.prototype.require = function (this: unknown, id: string) {
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const { createElement: h } = require("react");
const { renderToStaticMarkup } = require("react-dom/server");
const { default: SafeLink } = require("../src/ui/SafeLink");
const { NeedMenu } = require("../src/features/home/NeedsRail");
const { NeedRows } = require("../src/features/home/Overview");
const { Links } = require("../src/features/issues/Links");
const { PrPanel } = require("../src/features/issues/PrPanel");
const { MilestoneCheckpoint } = require("../src/features/projects/Milestones");

/**
 * CAD-1080: every surface that renders an externally-influenced href goes
 * through SafeLink, so a loopback href is a warning, never an anchor.
 */

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

const LOOPBACK = ["http://127.0.0.1:3010/x", "http://[::1]/x", "http://cadence-3010.localhost:4000/x"];
const GOOD = "https://github.com/o/r/pull/1";
const noAnchor = (html: string, href: string, what: string) => {
  equal(html.includes("<a "), false, `${what}: no anchor`);
  equal(html.includes(`href="${href}`), false, `${what}: no href attribute`);
  equal(html.includes("text-warn"), true, `${what}: warning span`);
};

// Unit: the helper.
const sl = (href: string | null | undefined) => renderToStaticMarkup(h(SafeLink, { href, className: "lnk" }, "t"));
equal(sl(GOOD), `<a class="lnk" href="${GOOD}" target="_blank" rel="noreferrer">t</a>`, "external anchor shape");
equal(sl("javascript:alert(1)"), "t", "unsafe scheme is plain text");
equal(sl("data:text/html,x"), "t", "data scheme is plain text");
equal(sl(""), "t", "empty href is plain text");
equal(sl(null), "t", "null href is plain text");
for (const l of LOOPBACK) {
  noAnchor(sl(l), l, `helper ${l}`);
  equal(sl(l).includes("not clickable on the board"), true, "warning title");
}

const need = (link: string | null) =>
  ({ key: "k", kind: "approval", label: "x", title: "t", owner: "pm", age: 1, subject: null, summary: null, issue: null, link, command: "c" }) as never;
const menu = (link: string | null) =>
  renderToStaticMarkup(
    h(NeedMenu, { need: need(link), block: null, busy: false, onOpenIssue: () => {}, onDecide: () => {}, onCopied: () => {}, onClose: () => {} }),
  );
const ovRows = (link: string | null) =>
  renderToStaticMarkup(
    h(NeedRows, { decision: false, rows: [{ kind: "approval", title: "ovt", age: 1, project: "demo", command: "c", link, audience: "operator" }] as never }),
  );
const detail = (url: string) =>
  ({ id: "CAD-1", rev: 1, refs: [{ kind: "pr", url, label: "PR #1" }, { kind: "url", url, label: "ref" }], links: { parent: null, children: [], blocked_by: [], blocks: [], relates: [], duplicate_of: null, duplicates: [] }, reports: [] }) as never;
const links = (url: string) =>
  renderToStaticMarkup(h(Links, { detail: detail(url), readOnly: true, hrefFor: (id: string) => `/i/${id}`, onWrite: (async () => {}) as never, onError: () => {} }));
const prPanel = (url: string) =>
  renderToStaticMarkup(h(PrPanel, { detail: detail(url), readOnly: true, onWrite: (async () => {}) as never, onError: () => {} }));
const ms = (entry: string) =>
  renderToStaticMarkup(
    h(MilestoneCheckpoint, { milestone: { project: "demo", id: "m", evidence: [entry], configured: true, progress: { done_weight: 1, total_weight: 1, ratio: 1, counts: { total: 1, done: 1, dropped: 0, open: 0, doing: 0, review: 0, blocked: 0 } }, issues: [], epics: [], health: { state: "on_track", reasons: [] } } as unknown as MilestoneRow, rows: [], onOpenIssue: () => {} }),
  );

const surfaces: Record<string, (href: string) => string> = {
  "needs menu": (u) => menu(u),
  overview: ovRows,
  "issue links": links,
  "pr panel": prPanel,
  milestone: ms,
};
for (const [name, render] of Object.entries(surfaces)) {
  for (const l of LOOPBACK) noAnchor(render(l), l, `${name} ${l}`);
  const ok = render(GOOD);
  equal(ok.includes(`<a `), true, `${name}: https stays an anchor`);
  equal(ok.includes(`href="${GOOD}"`) && ok.includes('target="_blank"') && ok.includes('rel="noreferrer"'), true, `${name}: https anchor attributes`);
}
equal(menu("javascript:alert(1)").includes("javascript:"), false, "needs menu drops javascript:");
// Non-anchor renderings keep the menu item semantics (role and item classes).
for (const l of [...LOOPBACK, "javascript:alert(1)"]) {
  equal(/<span[^>]*role="menuitem"/.test(menu(l)), true, `needs menu keeps menuitem role for ${l}`);
}

console.log("safe link checks passed");
