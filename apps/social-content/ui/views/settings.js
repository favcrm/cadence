/* module: ui/views/settings.js — Settings: client, protected terms,
 * disclaimer, timezone, drafting limit, default destinations. Edits are
 * in-memory via api.settings.* and feed back into the validators live. */
(function (SC) {
  "use strict";
  const { h } = SC.dom;

  SC.views = SC.views || {};
  SC.views.settings = function (el) {
    const api = SC.api;
    const c = api.clients.current();
    const s = api.settings.get();

    const termInput = h("input", { class: "field", placeholder: "add a term — kept verbatim",
      onkeydown: (e) => { if (e.key === "Enter" && e.target.value.trim()) { api.settings.addTerm(e.target.value.trim()); e.target.value = ""; } } });

    const termsCard = h("div", { class: "card setcard" },
      h("span", { class: "slabel" }, "protected terms"),
      h("div", { class: "small muted" }, "Every term found in a source post must survive into the caption verbatim — the check on each post enforces it."),
      h("div", { class: "terms" },
        s.protected_terms.map((t) => h("span", { class: "term" }, t,
          h("button", { title: "remove", onclick: () => api.settings.removeTerm(t) }, "×")))),
      termInput);

    const clientCard = h("div", { class: "card setcard" },
      h("span", { class: "slabel" }, "client"),
      h("div", { class: "kv" },
        h("span", { class: "k" }, "name"), h("span", null, c.name),
        h("span", { class: "k" }, "handle"), h("span", { class: "num" }, "@" + c.handle),
        h("span", { class: "k" }, "connectors"), h("span", null, c.connectors.join(", ")),
        h("span", { class: "k" }, "sources"), h("span", { class: "num" }, String(api.sources.list().length) + " posts in library"),
        h("span", { class: "k" }, "posts"), h("span", { class: "num" }, String(api.posts.list().length))),
      h("div", { class: "small muted" }, "Switch client from the header — every screen re-scopes."));

    const discInput = h("input", { class: "field", value: s.disclaimer, placeholder: "e.g. All prices in HKD",
      onchange: (e) => api.settings.update({ disclaimer: e.target.value }) });
    const tzSel = h("select", { class: "field", onchange: (e) => api.settings.update({ timezone: e.target.value }) },
      ["Asia/Hong_Kong", "Asia/Singapore", "Europe/London"].map((z) =>
        h("option", { value: z, selected: z === s.timezone }, z)));
    const limitInput = h("input", { class: "field", type: "number", min: "1", max: "50", value: s.drafting_limit,
      onchange: (e) => api.settings.update({ drafting_limit: Number(e.target.value) }) });

    const policyCard = h("div", { class: "card setcard" },
      h("span", { class: "slabel" }, "drafting policy"),
      h("div", { class: "kv" },
        h("span", { class: "k" }, "disclaimer"), discInput,
        h("span", { class: "k" }, "timezone"), tzSel,
        h("span", { class: "k" }, "draft limit"), limitInput,
        h("span", { class: "k" }, "destinations"), h("span", null, s.destinations.join(", "))),
      h("div", { class: "small muted" },
        "A disclaimer set here becomes a validator on every caption. Drafting runs under a standing approval: no sends, ≤ limit/day."));

    const wiringCard = h("div", { class: "card setcard" },
      h("span", { class: "slabel" }, "prototype"),
      h("div", { class: "small muted" },
        "All data is in-memory mock behind ui/mock/api.js. Wire-up swaps only that facade — views never see the store."),
      h("div", { class: "small muted" }, "Freeze ambient job motion: open with ?freeze"));

    el.append(
      h("div", { class: "pagehead" }, h("h1", null, "Settings")),
      h("div", { class: "setgrid" }, clientCard, termsCard, policyCard, wiringCard));
  };
})(globalThis.SC = globalThis.SC || {});
