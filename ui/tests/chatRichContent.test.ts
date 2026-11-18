import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import Md from "../src/ui/Md";
import { fileKind } from "../src/features/home/fileKind";

/**
 * CAD-2436: the renderer marks what CSS cannot tell apart. A paragraph is a
 * section label only when its whole text is one bold run; a bold lead-in
 * followed by text is a sentence. A table column of numbers is right-aligned.
 */

function check(ok: boolean, what: string): void {
  if (!ok) throw new Error(what);
}

const render = (text: string) => renderToStaticMarkup(createElement(Md, { text, onOpen: () => undefined, idTokens: true }));
const paragraphs = (html: string) => [...html.matchAll(/<p([^>]*)>([\s\S]*?)<\/p>/g)].map((m) => ({ attrs: m[1]!, body: m[2]! }));

// A bold-only paragraph is the label; a bold lead-in is not.
{
  const html = render("Intro line.\n\n**Where things stand**\n\n**Payment:** FPS / Bank Transfer, due on receipt.\n\n**Terms:** All prices in HKD.");
  const ps = paragraphs(html);
  const label = ps.find((p) => p.body.includes("Where things stand"));
  const payment = ps.find((p) => p.body.includes("Payment:"));
  const terms = ps.find((p) => p.body.includes("Terms:"));
  check(label?.attrs.includes("md-sect") === true, `bold-only paragraph is a section label: ${label?.attrs}`);
  check(payment !== undefined && !payment.attrs.includes("md-sect"), `a bold lead-in keeps body size: ${payment?.attrs}`);
  check(terms !== undefined && !terms.attrs.includes("md-sect"), `a second bold lead-in keeps body size: ${terms?.attrs}`);
  check(payment!.body.includes("<strong>Payment:</strong>"), "the lead-in keeps its bold");
  check(!ps[0]!.attrs.includes("md-sect"), "plain prose is not marked");
}

// Whitespace around a bold-only run still makes it a label; an empty bold does not.
{
  const ps = paragraphs(render("Text.\n\n  **Next steps**  \n\nMore."));
  check(ps.some((p) => p.body.includes("Next steps") && p.attrs.includes("md-sect")), "padded bold-only paragraph is a label");
}

// Numeric columns are right-aligned and marked for tabular figures; text columns are not.
{
  const html = render(
    "| Description | Qty | Unit price | Amount (HKD) | Code |\n|---|---|---|---|---|\n| Website update | 1.00 | 400.00 | HKD 400.00 | INV-0140 |\n| Handling fee | 1 | 40 | 40.00 | |",
  );
  const headers = [...html.matchAll(/<th([^>]*)>([^<]*)<\/th>/g)].map((m) => ({ attrs: m[1]!, text: m[2]! }));
  const numeric = (name: string) => headers.find((h) => h.text === name)?.attrs.includes("md-num") === true;
  check(!numeric("Description"), "a text column is not numeric");
  check(numeric("Qty") && numeric("Unit price") && numeric("Amount (HKD)"), `number columns are marked: ${JSON.stringify(headers)}`);
  check(!numeric("Code"), "an id column is not numeric");
  const cells = [...html.matchAll(/<td([^>]*)>([^<]*)<\/td>/g)].map((m) => ({ attrs: m[1]!, text: m[2]! }));
  const amount = cells.find((c) => c.text === "HKD 400.00");
  check(amount?.attrs.includes("md-num") === true, "a currency amount cell is marked");
  check(/text-align:\s*right|align="right"/.test(amount?.attrs ?? ""), `a numeric column is right-aligned: ${amount?.attrs}`);
}

// An explicit alignment from the author is kept.
{
  const html = render("| Item | Qty |\n|---|:-:|\n| A | 2 |");
  const qty = [...html.matchAll(/<th([^>]*)>Qty<\/th>/g)][0]?.[1] ?? "";
  check(/center/.test(qty), `the author's centre alignment is kept: ${qty}`);
}

// The tile's badge kind comes from the stored MIME type.
check(fileKind("application/pdf") === "pdf", "pdf");
check(fileKind("image/png") === "image", "image");
check(fileKind("video/mp4") === "video", "video");
check(fileKind("text/plain") === "text" && fileKind("application/json") === "text", "text");
check(fileKind("application/zip") === "file", "other");

console.log("chatRichContent: ok");
