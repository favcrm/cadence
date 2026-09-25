/* module: ui/views/workflows.js — the app's workflow stubs (plan templates),
 * shown read-only so the shape is reviewable before anything is wired. */
(function (SC) {
  "use strict";
  const { h } = SC.dom;

  SC.views = SC.views || {};
  SC.views.workflows = function (el) {
    const api = SC.api;
    el.append(
      h("div", { class: "pagehead" },
        h("h1", null, "Workflows"),
        h("span", { class: "note" }, "plan templates bundled with the app — each run is a job over records")),
      h("div", { class: "stack" },
        api.workflows.list().map((f) =>
          h("div", { class: "card wf" },
            h("div", { class: "row" },
              h("strong", { style: "color:var(--color-ink-100)" }, f.title),
              h("span", { class: "chip" }, f.id),
              h("span", { class: "grow" }),
              h("span", { class: "kicker" }, f.file)),
            h("div", { class: "steps" },
              f.steps.flatMap((s, i) => i ? [h("span", { class: "arr" }, "→"), h("span", { class: "st" }, s)] : [h("span", { class: "st" }, s)])),
            h("div", { class: "small muted" }, f.note)))));
  };
})(globalThis.SC = globalThis.SC || {});
