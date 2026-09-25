/* module: ui/views/runs.js — Runs: job list + per-item step progress. */
(function (SC) {
  "use strict";
  const { h, fmt } = SC.dom;

  const STEP_SEQ = ["adapt", "visuals", "review", "schedule", "verify"];
  const STEP_ICON = { done: "✓", running: "●", waiting: "", failed: "✕", mismatch: "≠", skipped: "–" };

  function stepsEl(steps) {
    const seq = Object.keys(steps)[0] === "change" ? ["change"] : STEP_SEQ;
    return h("div", { class: "steps" },
      seq.map((s) => h("span", { class: "step " + steps[s] },
        h("span", { class: "st-ic" }, STEP_ICON[steps[s]] || ""), s)));
  }

  SC.views = SC.views || {};
  SC.views.runs = function (el) {
    const api = SC.api, w = SC.w;
    const runs = api.runs.list()
      .filter((r) => r.client === api.clients.current().id || r.kind === "revise")
      .sort((a, b) => (a.at < b.at ? 1 : -1));

    el.append(
      h("div", { class: "pagehead" },
        h("h1", null, "Runs"),
        h("span", { class: "note" }, "workflow runs over records — each item pinned to the post it produced")),
      h("div", { class: "stack" },
        runs.map((r) => h("div", { class: "card run" },
          h("div", { class: "rhead" },
            h("span", { class: "rid" }, r.id),
            h("span", { class: "chip" }, r.kind),
            h("span", { class: "chip " + (r.status === "done" ? "ok" : "info") }, r.status),
            h("span", { class: "muted small" }, r.label),
            h("span", { class: "grow" }),
            h("span", { class: "kicker" }, fmt.ago(r.at))),
          h("div", { class: "ritems" },
            r.items.map((it) => {
              const p = api.posts.get(it.post);
              return h("div", { class: "ritem" },
                w.statusDot(p ? p.status : "drafting"),
                h("button", { class: "lnk who", style: "text-align:left",
                  onclick: () => (location.hash = "#/post/" + it.post) },
                  `${it.post} — ${p && p.caption.text ? p.caption.text.slice(0, 60) : "(no caption yet)"}`),
                stepsEl(it.steps));
            })),
          h("div", { class: "runlog" },
            r.log.slice(-6).map(([t, m]) => h("div", { class: "l" },
              h("span", { class: "t num" }, fmt.ago(t)), h("span", null, m))))))));
  };
})(globalThis.SC = globalThis.SC || {});
