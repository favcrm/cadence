import { splitFrontmatter, wikiLinkHref } from "../src/features/wiki/citations";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

// CAD-1917 item 1: only a block with at least one `key: value` pair is metadata.
const noPairs = "---\njust a note\n---\nBody\n";
equal(splitFrontmatter(noPairs), { meta: [], body: noPairs }, "a block without pairs stays in the body");
// A YAML list under a key keeps its items.
equal(
  splitFrontmatter("---\ntitle: T\ntags:\n  - a\n  - b\n---\nBody\n"),
  { meta: [["title", "T"], ["tags", "a, b"]], body: "Body\n" },
  "list items join under their key",
);

// Item 3: a relative link keeps its fragment, query and hash.
equal(wikiLinkHref("global/a/b.md", "c.md#heading"), "/wiki/global/a/c.md#heading", "fragment kept");
equal(wikiLinkHref("global/a/b.md", "c.md?at=" + "a".repeat(40) + "#h"), "/wiki/global/a/c.md?at=" + "a".repeat(40) + "#h", "query and hash kept");

// Item 7: the wiki root opens in place.
equal(wikiLinkHref("global/a/b.md", "/wiki"), "/wiki", "/wiki");
equal(wikiLinkHref("global/a/b.md", "/wiki/"), "/wiki", "/wiki/");
equal(wikiLinkHref("global/a/b.md", "../../"), "/wiki", "../../ to the wiki root");

// Confinement and item 6.
for (const bad of [
  "/", "/settings", "../../..", "..%2fsettings", "/wiki/..%2fsettings", "/wiki/%09../settings", "/wiki/a%5cb",
  "/wiki/a%2fb", "/wiki/a%00b", "/wikifoo", "https://x.test/wiki/a", "//evil.test/wiki/a", "javascript:alert(1)", "#top",
]) equal(wikiLinkHref("global/a/b.md", bad), null, `refused ${bad}`);
equal(wikiLinkHref("global/a/b.md", "%2e%2e/%2e%2e/%2e%2e/x"), null, "encoded dot segments cannot climb out");
