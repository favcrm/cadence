import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import Md from "../src/ui/Md";

/**
 * CAD-1037: bare issue ids become `issue:` links in prose only. Fenced
 * blocks, indented code, inline code, existing links and URLs stay verbatim.
 */

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

const render = (text: string) => renderToStaticMarkup(createElement(Md, { text }));
const linked = (html: string) => html.includes('class="lnk num"');

// Prose id is linked.
const prose = render("see CAD-12 now");
equal(linked(prose), true, "prose id becomes a link button");
equal(prose.includes(">CAD-12</button>"), true, "link text is the id");

// Code blocks of every flavour keep the id verbatim and unlinked.
const blocks: Record<string, string> = {
  "backtick fence": "```\nrun CAD-12\n```",
  "tilde fence": "~~~\nrun CAD-12\n~~~",
  "fence with language": "```sh\ncadence issue show CAD-12\n```",
  "long fence containing a short one": "````\n```\nCAD-12\n```\n````",
  "indented code": "para\n\n    cadence issue show CAD-12\n",
};
for (const [name, md] of Object.entries(blocks)) {
  const html = render(md);
  equal(/<pre><code[^>]*>/.test(html), true, `${name}: renders a code block`);
  equal(html.includes("CAD-12"), true, `${name}: id is present`);
  equal(linked(html), false, `${name}: no link button`);
  equal(html.includes("["), false, `${name}: no markdown link syntax leaks`);
  equal(html.includes("issue:"), false, `${name}: no issue: target leaks`);
}

// Prose after a fenced block is still linked.
equal(linked(render("```\nCAD-1\n```\n\nthen CAD-2")), true, "prose after a fence is linked");

// Inline code stays unlinked.
const inline = render("run `CAD-12` please");
equal(inline.includes("<code>CAD-12</code>"), true, "inline code keeps the id");
equal(linked(inline), false, "inline code is not linked");

// An id inside an existing link is not double-wrapped.
const inLink = render("[CAD-12](https://example.com/x)");
equal(linked(inLink), false, "existing link is not turned into an issue link");
equal(inLink.includes('href="https://example.com/x"'), true, "existing link keeps its target");
equal(inLink.includes("issue:"), false, "no nested issue: link");

// An id in a URL is untouched.
const url = render("open https://example.com/CAD-12 and [x](https://e.com/CAD-12)");
equal(url.includes('href="https://example.com/CAD-12"'), true, "autolinked URL keeps its path");
equal(linked(url), false, "ids in URLs are not issue links");

console.log("md issue link checks passed");
