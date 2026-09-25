/* module: ui/views/home.js — Home: week calendar + status board +
 * suggestions rail (design doc §6 view map). */
(function (SC) {
  "use strict";
  const { h, fmt } = SC.dom;

  SC.views = SC.views || {};
  SC.views.home = function (el) {
    const api = SC.api, w = SC.w;
    const posts = api.posts.list();

    /* --- week calendar: current Mon–Sun (HKT), posts by schedule_at --- */
    const now = api.NOW();
    const days = fmt.weekOf(now.toISOString());
    const todayKey = fmt.dkey(now.toISOString());
    const cal = h("div", { class: "week-wrap" }, h("div", { class: "week" },
      days.map((d) => {
        const dkey = fmt.hkDayKey(d);
        const dayPosts = posts.filter((p) => fmt.dkey(p.scheduleAt) === dkey)
          .sort((a, b) => (a.scheduleAt < b.scheduleAt ? -1 : 1));
        const today = dkey === todayKey;
        return h("div", { class: "daycol" + (today ? " today" : "") },
          h("div", { class: "dayhead" },
            h("span", { class: "dow" }, fmt.day(d.toISOString()).split(" ")[0]),
            h("span", { class: "dnum num" }, String(Number(dkey.split("/")[0])))),
          dayPosts.map((p) => h("div", {
            class: "calpost",
            style: `border-left-color:${w.STATUS[p.status].color}`,
            onclick: () => (location.hash = "#/post/" + p.id),
            title: p.caption.text || "(no caption)",
          },
            h("div", { class: "t num" }, fmt.time(p.scheduleAt).split(" ").pop()),
            h("div", { class: "cap" }, p.caption.text || "being drafted…"))));
      })));

    /* --- board: one column per state --- */
    const cols = ["drafting", "in_review", "ready", "waiting", "scheduled", "published", "needs_you"];
    const board = h("div", { class: "board" },
      cols.map((s) => {
        const inCol = posts.filter((p) => p.status === s);
        if (s === "needs_you" && !inCol.length) return null;
        return h("div", { class: "bcol" },
          h("div", { class: "bhead" },
            h("span", { class: "slabel" }, w.statusDot(s), w.STATUS[s].label),
            h("span", { class: "count num" }, String(inCol.length))),
          h("div", { class: "bcards" },
            inCol.length ? inCol.map(w.postCard) : h("div", { class: "muted small", style: "padding:2px 4px" }, "—")));
      }));

    /* --- suggestions rail --- */
    const suggs = api.suggestions.list();
    const rail = h("div", { class: "rail" },
      h("span", { class: "slabel" }, `suggestions · ${suggs.length}`),
      suggs.length ? suggs.map(w.suggCard)
        : h("div", { class: "card sugg" }, h("div", { class: "small muted" }, "No pending proposals — when an agent wants to change a field you edited, it asks here.")));

    el.append(
      h("div", { class: "home-grid" },
        h("div", null,
          h("section", { class: "sect" }, h("span", { class: "slabel" }, "this week"), cal),
          h("section", { class: "sect" }, h("span", { class: "slabel" }, "board"), board)),
        rail));
  };
})(globalThis.SC = globalThis.SC || {});
