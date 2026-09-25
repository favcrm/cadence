/* module: ui/views/automations.js — Automations: triggers that run under
 * standing approvals (§5.1 row 3). Toggles are in-memory only. */
(function (SC) {
  "use strict";
  const { h } = SC.dom;

  SC.views = SC.views || {};
  SC.views.automations = function (el) {
    const api = SC.api;
    el.append(
      h("div", { class: "pagehead" },
        h("h1", null, "Automations"),
        h("span", { class: "note" }, "scheduled/event triggers — they propose; sends still wait for the digest")),
      h("div", { class: "stack" },
        api.automations.list().map((a) =>
          h("div", { class: "card auto" },
            h("button", {
              class: "toggle" + (a.on ? " on" : ""), role: "switch",
              "aria-checked": String(a.on), "aria-label": a.name,
              onclick: () => api.automations.toggle(a.id),
            }),
            h("div", null,
              h("div", { class: "nm" }, a.name),
              h("div", { class: "sub" }, a.desc)),
            h("div", { class: "aside", style: "text-align:right" },
              h("div", { class: "small mono", style: "color:var(--color-ink-300)" }, a.every),
              h("div", { class: "kicker" }, `last: ${a.last}`),
              h("div", { class: "kicker" }, a.approval))))));
  };
})(globalThis.SC = globalThis.SC || {});
