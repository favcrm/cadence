import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import Md from "../src/ui/Md";
import { previewKind } from "../src/features/wiki/preview";

/**
 * CAD-581's XSS probes on the wiki's markdown path. The page view renders
 * through the board's Md component (react-markdown, no raw-HTML plugin);
 * these render that component to static markup and assert that the three
 * classic injections never survive: a <script> tag, an onerror handler on
 * raw HTML, and a javascript: link.
 */

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

const render = (text: string) => renderToStaticMarkup(createElement(Md, { text }));

// Probe 1 — a script tag in a page's markdown is text, never an element.
const script = render("<script>alert(1)</script>");
equal(/<script/i.test(script), false, "no script element is produced");
equal(script.includes("&lt;script&gt;alert(1)&lt;/script&gt;"), true, "the tag renders escaped as text");

// Probe 2 — raw HTML with an event handler is text too.
const onerror = render('<img src=x onerror="alert(1)">');
equal(/<img/i.test(onerror), false, "no img element is produced");
equal(onerror.includes("&lt;img"), true, "the tag renders escaped as text");
const svg = render('<svg onload="alert(1)"></svg>');
equal(/<svg/i.test(svg), false, "no svg element is produced");

// Probe 3 — a javascript: link loses its target.
const js = render("[click](javascript:alert(1))");
equal(/javascript:/i.test(js), false, "the javascript: url is stripped");
equal(js.includes('href=""'), true, "the anchor renders without a target");
equal(js.includes(">click</a>"), true, "the link's text still shows");
const mixed = render("[x](JaVaScRiPt:alert(1))");
equal(/javascript:/i.test(mixed), false, "case does not smuggle the scheme through");
const img = render("![x](javascript:alert(1))");
equal(/javascript:/i.test(img), false, "an image src cannot be a javascript: url");
const data = render("[x](data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==)");
equal(/data:text\/html/i.test(data), false, "a data:text/html link is stripped too");

// Probe 4 — defence in depth: an uploaded SVG or HTML never previews
// inline, whatever MIME the server sniffed (the preview pane downloads).
equal(previewKind("evil.svg", "image/svg+xml"), "download", "an uploaded svg downloads");
equal(previewKind("evil.html", "text/html"), "download", "an uploaded html downloads");
equal(previewKind("evil.xhtml", null), "download", "an uploaded xhtml downloads");

// And the safe cases still render as themselves.
const ok = render("**bold** and a [link](https://example.com/)");
equal(ok.includes("<strong>bold</strong>"), true, "markdown still renders");
equal(ok.includes('href="https://example.com/"'), true, "a normal link keeps its target");

console.log("wiki xss checks passed");
