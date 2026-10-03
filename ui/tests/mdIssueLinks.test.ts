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

// A reference-style link keeps its text: the id inside is not an issue link.
const ref = render("[see CAD-1][r]\n\n[r]: https://example.com");
equal(linked(ref), false, "reference-link text is not an issue link");
equal(ref.includes('href="https://example.com"'), true, "reference link keeps its target");
equal(ref.includes("issue:"), false, "no issue: target in a reference link");

// The `a` component only opens well-formed ids (CAD-1070).
type El = { type: unknown; props: Record<string, any> };
function anchor(href: string, onOpen: (id: string) => void): El | null {
  const el = Md({ text: "x", onOpen }) as unknown as El;
  return el.props.components.a({ href, children: "x" });
}
const opened: string[] = [];
const good = anchor("issue:CAD-12", (id) => opened.push(id)) as El;
equal(good.type, "button", "valid id renders a button");
good.props.onClick();
equal(opened, ["CAD-12"], "valid id reaches onOpen");
for (const bad of ["issue:javascript:alert(1)", "issue:", "issue:cad-12", "issue:CAD-12/../x", "issue:CAD-12 ", "issue:A-1"]) {
  const el = anchor(bad, (id) => opened.push(id)) as unknown as { type: unknown; props: Record<string, any> };
  equal(el.type === "button" || el.type === "a", false, `${bad}: no button or anchor`);
  equal(el.props.children, "x", `${bad}: link text kept as plain text`);
}
equal(opened, ["CAD-12"], "hostile ids never call onOpen");
// End to end: a hostile target renders no button and no anchor.
const hostile = render("[x](issue:javascript:alert(1)) and [y](issue:cad-12)");
equal(/<button|<a /.test(hostile), false, "hostile issue: links render as text");
equal(hostile.includes("javascript"), false, "hostile target does not leak into markup");

console.log("md issue link checks passed");
