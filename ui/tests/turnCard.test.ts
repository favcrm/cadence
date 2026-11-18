import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { TurnCard } from "../src/ui/TurnCard";

/** CAD-2440: the card system's shared frame (CAD-2437 phase 0). */

function check(ok: boolean, what: string): void {
  if (!ok) throw new Error(`turnCard: ${what}`);
}

// A full region: name, kind, header parts, status as text, body, footer.
{
  const hooks = { "data-kind": "form" };
  const html = renderToStaticMarkup(
    createElement(
      TurnCard,
      {
        label: "Form from the assistant: Weekly review",
        kind: "form",
        className: "card agentform",
        bodyClassName: "agentform-body",
        icon: createElement("svg", { "aria-hidden": "true" }),
        kindLabel: "Form",
        subject: "Weekly review",
        status: { text: "Waiting for you", tone: "accent", attrs: { "data-form-status": "open" } },
        footer: createElement("button", { type: "button" }, "Send"),
        ...hooks,
      },
      createElement("p", null, "Body"),
    ),
  );
  check(html.startsWith("<section"), "a region element");
  check(html.includes('aria-label="Form from the assistant: Weekly review"'), "the region is named");
  check(html.includes('class="turncard card agentform"'), "the caller keeps its own frame classes");
  check(html.includes('data-card-kind="form"') && html.includes('data-kind="form"'), "kind and the caller's hook");
  check(html.includes('data-density="full"'), "full density by default");
  check(html.includes('class="turncard-body agentform-body"'), "the body keeps the caller's class");
  check(/class="turncard-kind"><svg[^>]*><\/svg>Form<\/span><span class="turncard-subject">Weekly review<\/span><span class="turncard-status"/.test(html), "kind icon and label, then the subject, then the status");
  check(
    html.includes('<span class="turncard-status" data-tone="accent" data-form-status="open">Waiting for you</span>'),
    "status spoken as text with its tone and hook",
  );
  check(html.includes("<p>Body</p>"), "body content");
  check(html.includes('<div class="turncard-foot"><button type="button">Send</button></div>'), "footer actions");
}

// A bare region: no header, no footer, compact. With only a subject, the header is just the subject.
{
  const html = renderToStaticMarkup(createElement(TurnCard, { label: "Note", kind: "note", density: "compact" }, "x"));
  check(!html.includes("turncard-head"), "no header without icon, kind label or status");
  check(!html.includes("turncard-foot"), "no footer without actions");
  check(html.includes('data-density="compact"'), "compact density");
  const subjectOnly = renderToStaticMarkup(createElement(TurnCard, { label: "Note", kind: "note", subject: "Plan" }, "x"));
  check(subjectOnly.includes('<div class="turncard-head"><span class="turncard-subject">Plan</span></div>'), "a subject alone makes the header");
}

// The summary button: tile, title, meta, trailing cue, open state.
{
  const html = renderToStaticMarkup(
    createElement(TurnCard, {
      shape: "button",
      kind: "artifact",
      icon: "HTML",
      subject: "Invoice template",
      meta: createElement("span", { className: "artmeta" }, "v1 · 4 KB"),
      trailing: createElement("span", { className: "artopen" }, "Open"),
      open: true,
      "aria-label": "Open Invoice template",
    }),
  );
  check(html.startsWith('<button type="button"'), "a real button by default");
  check(html.includes('class="turncard-button artcard"'), "keeps the artcard frame class");
  check(html.includes('data-open="true"'), "the open state");
  check(html.includes('<span class="turncard-tile arttype">HTML</span>'), "the kind tile");
  check(html.includes('<span class="turncard-text arttext"><b>Invoice template</b><span class="artmeta">v1 · 4 KB</span></span>'), "title and meta");
  check(html.includes('<span class="artopen">Open</span>'), "the trailing cue");
}

// A caller that needs a role="button" element gets a div.
{
  const html = renderToStaticMarkup(
    createElement(TurnCard, { shape: "button", kind: "artifact", element: "div", role: "button", tabIndex: 0, icon: "MD", subject: "Notes" }),
  );
  check(html.startsWith("<div") && html.includes('role="button"') && html.includes('tabindex="0"'), "div with button role");
  check(!html.includes("data-open"), "no open attribute when closed");
}

// The frame must not reuse the Projects board's clickable `.tcard` (its
// pointer cursor and hover background would land on a form).
{
  const all = [
    renderToStaticMarkup(createElement(TurnCard, { label: "x", kind: "form", icon: "i", status: { text: "s", tone: "ok" }, footer: "f" }, "b")),
    renderToStaticMarkup(createElement(TurnCard, { shape: "button", kind: "artifact", icon: "T", subject: "t" })),
  ].join("");
  check(!/class="[^"]*\btcard\b/.test(all), "no Projects tcard class on the chat frame");
}

console.log("turnCard: ok");
