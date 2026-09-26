/* wiki mockup — hash router + small interactions. Views are static
 * <section data-view> blocks; the router shows one and marks the tree
 * row that led there. */

(function () {
  "use strict";

  var views = Array.prototype.slice.call(document.querySelectorAll("[data-view]"));
  var rows = Array.prototype.slice.call(document.querySelectorAll("#wtree .trow[data-route]"));

  function route() {
    var h = location.hash.replace(/^#\/?/, "");
    return h || "page";
  }

  function render() {
    var name = route();
    var found = false;
    views.forEach(function (s) {
      var on = s.getAttribute("data-view") === name;
      s.hidden = !on;
      if (on) found = true;
    });
    if (!found) {
      views.forEach(function (s) { s.hidden = s.getAttribute("data-view") !== "page"; });
      name = "page";
    }
    rows.forEach(function (r) {
      r.classList.toggle("on", r.getAttribute("data-route") === "#/" + name);
    });
    var wp = document.getElementById("wpane");
    if (wp) wp.scrollTop = 0;
  }

  addEventListener("hashchange", render);
  render();

  // Tree rows navigate; the caret also toggles a folder's open state.
  document.getElementById("wtree").addEventListener("click", function (e) {
    var caret = e.target.closest(".caret");
    var row = e.target.closest(".trow[data-route]");
    if (caret && row && row.classList.contains("dir")) {
      row.classList.toggle("open");
      e.stopPropagation();
      return;
    }
    if (row) location.hash = row.getAttribute("data-route");
  });

  // Tiles and list rows with a target navigate.
  document.addEventListener("click", function (e) {
    var t = e.target.closest("[data-goto]");
    if (t && !e.target.closest(".fmenu-btn") && !e.target.closest(".fmenu")) {
      location.hash = t.getAttribute("data-goto");
    }
  });

  // Folder view: grid/list toggle.
  var vg = document.getElementById("vgrid"), vl = document.getElementById("vlist");
  var grid = document.getElementById("folder-grid"), list = document.getElementById("folder-list");
  function setMode(listMode) {
    grid.hidden = listMode; list.hidden = !listMode;
    vg.classList.toggle("on", !listMode); vl.classList.toggle("on", listMode);
  }
  if (vg && vl) {
    vg.addEventListener("click", function () { setMode(false); });
    vl.addEventListener("click", function () { setMode(true); });
  }

  // Per-item menus — one open at a time, outside click closes.
  document.addEventListener("click", function (e) {
    var btn = e.target.closest("[data-menu]");
    var open = document.querySelectorAll(".fmenu:not([hidden])");
    if (!btn) {
      open.forEach(function (m) { m.hidden = true; });
      document.querySelectorAll(".fmenu-btn.on").forEach(function (b) { b.classList.remove("on"); });
      return;
    }
    e.stopPropagation();
    var menu = btn.parentElement.querySelector(".fmenu");
    var wasOpen = menu && !menu.hidden;
    open.forEach(function (m) { m.hidden = true; });
    document.querySelectorAll(".fmenu-btn.on").forEach(function (b) { b.classList.remove("on"); });
    if (menu && !wasOpen) { menu.hidden = false; btn.classList.add("on"); }
  });

  // Theme toggle — system → light → dark, stored as "cadence-theme"
  // (identical to kit.html and the board's lib/theme.ts).
  var KEY = "cadence-theme";
  var icons = {
    system: '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4"><circle cx="8" cy="8" r="5.5"/><path d="M8 2.5v11a5.5 5.5 0 0 0 0-11z" fill="currentColor"/></svg>',
    light: '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4"><circle cx="8" cy="8" r="3"/><path d="M8 1.5v1.8M8 12.7v1.8M1.5 8h1.8M12.7 8h1.8M3.4 3.4l1.3 1.3M11.3 11.3l1.3 1.3M12.6 3.4l-1.3 1.3M4.7 11.3l-1.3 1.3"/></svg>',
    dark: '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4"><path d="M13.5 9.6A5.8 5.8 0 0 1 6.4 2.5a5.8 5.8 0 1 0 7.1 7.1z"/></svg>'
  };
  function pref() {
    try { var v = localStorage.getItem(KEY); return v === "light" || v === "dark" ? v : "system"; }
    catch (e) { return "system"; }
  }
  var next = { system: "light", light: "dark", dark: "system" };
  var btn = document.getElementById("theme-toggle");
  function paint(p) {
    btn.innerHTML = icons[p];
    btn.title = "theme: " + p + " — switch to " + next[p];
    btn.setAttribute("aria-label", btn.title);
  }
  paint(pref());
  btn.addEventListener("click", function () {
    var p = next[pref()];
    try { if (p === "system") localStorage.removeItem(KEY); else localStorage.setItem(KEY, p); } catch (e) {}
    if (p === "system") document.documentElement.removeAttribute("data-theme");
    else document.documentElement.setAttribute("data-theme", p);
    paint(p);
  });
})();
