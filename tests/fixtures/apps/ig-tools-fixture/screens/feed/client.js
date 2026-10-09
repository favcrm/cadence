// ig-tools-fixture feed screen — standalone tool bridge proof (CAD-1177).
// Declares instagram.read -> source; calls the host bridge's `tool` op.
(function () {
  "use strict";
  var api = window.CADENCE_SCREEN;
  function render(posts) {
    var el = document.getElementById("feed");
    if (!el) return;
    el.textContent = posts && posts.length ? posts.map(function (p) {
      return (p && (p.code || p.id)) || "post";
    }).join("\n") : "no posts";
  }
  function load() {
    if (!api || !api.actions || !api.actions.invoke) return;
    api.actions.invoke("instagram.read", { profile_handle: "juicysuite_crm" },
      "req-" + Date.now()).then(function (r) {
        var items = r && r.result && (r.result.items || r.result.posts);
        render(items || []);
      }).catch(function () { render([]); });
  }
  if (api && api.on) { api.on("init", load); } else { load(); }
}());
