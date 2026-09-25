/* module: ui/app.js — shell: topbar (title, client switcher, blog button,
 * theme), tab nav, hash router. Re-renders the current route on any api
 * change; preserves focus in inputs across renders via data-fid.
 */
(function (SC) {
  "use strict";
  const { h, icons } = SC.dom;

  const ROUTES = [
    ["home", "Home", icons.home],
    ["library", "Library", icons.library],
    ["runs", "Runs", icons.runs],
    ["needs", "Needs you", icons.needs],
    ["automations", "Automations", icons.autos],
    ["workflows", "Workflows", icons.flows],
    ["settings", "Settings", icons.gear],
  ];

  /* theme — same convention as the board: data-theme on <html>, the
   * cadence-theme key, system → light → dark cycle. */
  const THEME_KEY = "cadence-theme";
  function applyTheme(pref) {
    if (pref === "system") document.documentElement.removeAttribute("data-theme");
    else document.documentElement.setAttribute("data-theme", pref);
  }
  function themePref() {
    try { const v = localStorage.getItem(THEME_KEY); return v === "light" || v === "dark" ? v : "system"; }
    catch { return "system"; }
  }
  let pref = themePref();
  applyTheme(pref);
  function cycleTheme() {
    pref = pref === "system" ? "light" : pref === "light" ? "dark" : "system";
    try { pref === "system" ? localStorage.removeItem(THEME_KEY) : localStorage.setItem(THEME_KEY, pref); } catch {}
    applyTheme(pref); renderChrome();
  }

  function route() {
    const m = (location.hash || "#/home").match(/^#\/([a-z-]+)(?:\/(.+))?$/);
    return { name: m ? m[1] : "home", arg: m && m[2] };
  }

  /* ---------- chrome ---------- */
  function renderChrome() {
    const api = SC.api, w = SC.w;
    const c = api.clients.current();
    const r = route();
    const needs = api.needsYou.list().length;

    const menu = h("div", { class: "menu", style: "display:none" },
      api.clients.list().map((cl) => h("button", {
        onclick: () => { api.clients.switch(cl.id); menu.style.display = "none"; },
      },
        h("span", { class: "avatar", style: `background:${cl.color}` }, cl.initials),
        h("span", null, cl.name),
        h("span", { class: "meta" }, "@" + cl.handle))));
    const clientBtn = h("button", { class: "client-btn", onclick: (e) => { e.stopPropagation(); menu.style.display = menu.style.display === "none" ? "block" : "none"; } },
      h("span", { class: "avatar", style: `background:${c.color}` }, c.initials),
      c.name, "▾");
    document.addEventListener("click", () => (menu.style.display = "none"));

    const themeBtn = h("button", { class: "btn sm", title: `theme: ${pref} (click to cycle)`, onclick: cycleTheme },
      pref === "dark" ? "◐ dark" : pref === "light" ? "◑ light" : "◌ system");

    const blogBtn = h("button", { class: "btn sm blogbtn", onclick: () =>
      w.modal("Write a blog post",
        h("div", { class: "small muted" },
          "This action sits outside the drafting standing approval, so wired-up it opens a plan card first ",
          "(inputs → approve → run the blog-post workflow). In the prototype it only shows this card."),
        h("div", { class: "card", style: "padding:10px 12px" },
          h("div", { class: "small" }, h("strong", null, "blog-post"), " — plan preview"),
          h("div", { class: "small muted", style: "margin-top:4px" },
            "input: brief → outline → draft → images → review — see workflows/blog-post.md")),
        h("div", { class: "row", style: "justify-content:flex-end" },
          h("button", { class: "btn sm", onclick: (e) => e.target.closest(".scrim").remove() }, "Not now"),
          h("button", { class: "btn sm primary", onclick: (e) => { e.target.closest(".scrim").remove(); w.toast("prototype: plan approval would dispatch the workflow here", true); } }, "Approve & run (mock)")))
    }, "Write a blog post");

    const top = h("header", { class: "topbar" },
      h("div", { class: "logo" }, icons.flows.cloneNode ? icons.flows : "",
        "Content studio", h("span", { class: "sub" }, "social-content · prototype")),
      h("div", { class: "client-switch" }, clientBtn, menu),
      h("span", { class: "spacer" }),
      blogBtn, themeBtn);

    const tabs = h("nav", { class: "tabs" },
      ROUTES.map(([name, label]) => h("a", {
        class: "tab", href: "#/" + name,
        "aria-current": r.name === name ? "page" : null,
      }, label, name === "needs" && needs ? h("span", { class: "badge" }, String(needs)) : null)));

    document.getElementById("chrome").replaceChildren(top, tabs);
  }

  /* ---------- render ---------- */
  const VIEWS = () => ({
    home: SC.views.home, library: SC.views.library, runs: SC.views.runs,
    needs: SC.views.needs, automations: SC.views.automations,
    workflows: SC.views.workflows, settings: SC.views.settings,
  });

  let pending = false;
  function render() {
    renderChrome();
    const view = document.getElementById("view");
    // preserve the input being typed in across ambient re-renders
    const act = document.activeElement;
    const fid = act && act.dataset && act.dataset.fid;
    const keep = fid ? { fid, v: act.value, s: act.selectionStart, e: act.selectionEnd } : null;
    view.replaceChildren();
    const r = route();
    const fn = r.name === "post" ? (el) => SC.views.post(el, r.arg) : VIEWS()[r.name];
    const wrap = h("div", { class: "reveal" });
    if (fn) fn(wrap); else wrap.append(h("div", { class: "pagehead" }, h("h1", null, "not found")));
    view.append(wrap);
    if (keep) {
      const again = view.querySelector(`[data-fid="${keep.fid}"]`);
      if (again && !again.disabled) { again.focus(); if (again.setSelectionRange && keep.s != null) { if (again.value === keep.v) again.setSelectionRange(keep.s, keep.e); else again.value = keep.v; } }
    }
  }
  function scheduleRender() {
    if (pending) return;
    pending = true;
    requestAnimationFrame(() => { pending = false; render(); });
  }

  SC.app = { rerender: scheduleRender, render };

  window.addEventListener("hashchange", render);
  document.addEventListener("DOMContentLoaded", () => {
    SC.api.on(scheduleRender);
    if (!location.hash) location.hash = "#/home";
    render();
  });
})(globalThis.SC = globalThis.SC || {});
