/* module: ui/views/library.js — Library: source posts grid, multi-select,
 * "Draft N posts" (§5.1 start a job). */
(function (SC) {
  "use strict";
  const { h, fmt, mediaEl } = SC.dom;

  let newOnly = true;

  SC.views = SC.views || {};
  SC.views.library = function (el) {
    const api = SC.api, w = SC.w;
    let list = api.sources.list();
    const sel = new Set(api.sources.selectedIds());
    const shown = newOnly ? list.filter((s) => s.isNew) : list;

    const bar = h("div", { class: "libbar" },
      h("button", {
        class: "btn sm" + (newOnly ? " primary" : ""),
        onclick: () => { newOnly = !newOnly; SC.app.rerender(); },
      }, newOnly ? "new only ✓" : "new only"),
      h("span", { class: "selinfo" }, sel.size ? `${sel.size} selected` : `${shown.length} of ${list.length} shown`),
      h("span", { class: "grow" }),
      sel.size ? h("button", { class: "btn sm", onclick: () => api.sources.clear() }, "clear") : null,
      h("button", {
        class: "btn primary", disabled: !sel.size,
        onclick: () => {
          const ids = api.sources.selectedIds();
          const rid = api.runs.draft(ids);
          w.toast(`${rid} started — drafting ${ids.length} post(s). See Runs.`, true);
          location.hash = "#/runs";
        },
      }, sel.size ? `Draft ${sel.size} post${sel.size > 1 ? "s" : ""}` : "Draft posts"));

    const grid = h("div", { class: "grid" },
      shown.map((s) => h("div", {
        class: "card tcard src" + (sel.has(s.id) ? " sel" : ""),
        onclick: () => api.sources.toggle(s.id),
      },
        h("div", { class: "thumb" },
          mediaEl(s.media[0] && s.media[0].seed),
          h("span", { class: "pick" }, sel.has(s.id) ? "✓" : "")),
        h("div", { class: "body" },
          h("div", { class: "txt" }, s.text),
          h("div", { class: "meta" },
            w.platformChip(s.platform),
            s.isNew ? h("span", { class: "chip acc" }, h("span", { class: "newdot" }), "new") : null,
            h("span", { class: "ago num" }, fmt.ago(s.postedAt)))))));

    el.append(
      h("section", { class: "sect" },
        h("span", { class: "slabel" }, `source posts — ${api.clients.current().name}`), bar, grid));
  };
})(globalThis.SC = globalThis.SC || {});
