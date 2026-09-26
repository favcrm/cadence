/* CAD-605 mockup — hash routes, kickoff pickers, theme toggle.
 * Nothing here talks to a daemon. Confirmations only describe the click. */
(function () {
  "use strict";

  var MODELS = {
    cursor: [{ id: "grok-4.7-high", cost: "Cursor plan" }],
    devin: [
      { id: "swe-2-medium", cost: "Free (quota)" },
      { id: "swe-2-high", cost: "Free (quota)" },
      { id: "swe-2-max", cost: "Free (quota)" }
    ],
    openrouter: [{ id: "openrouter/example-flash", cost: "Paid" }]
  };
  var PROVIDER_LABEL = { cursor: "Cursor", devin: "Devin", openrouter: "OpenRouter" };
  var DEFAULT_EFFORT = { cursor: "high", devin: "max", openrouter: "medium" };
  var LANES = ["none", "lane", "fenced", "quota", "blocked", "shipped"];
  var TABS = ["overview", "activity", "conversation", "pr", "evidence"];
  var DIALOGS = ["kickoff", "confirm", "plan", "unfence", "reassign", "interrupt", "approve", "master"];

  var BRIEF = [
    "CAD-604 — Issue detail page",
    "",
    "Replace the drawer with a page: read the goal, kick off one lane, talk to the agent, and follow the work through rollout.",
    "",
    "Acceptance",
    "- Header actions: Kick off, Ask agent, Approve.",
    "- Kick off picks provider, model, effort, and PM group, then shows this brief.",
    "- One timeline from claimed through rollout.",
    "- Ask for status and Instruction are separate composer modes.",
    "- Approve merge is operator-only and human class."
  ].join("\n");

  var CHECKS = {
    none: [0, 0, 0, 0, 0],
    lane: [1, 1, 1, 0, 0],
    fenced: [1, 1, 0, 0, 0],
    quota: [1, 1, 1, 0, 0],
    shipped: [1, 1, 1, 1, 1]
  };
  var FIELDS = {
    none: { status: "doing", priority: "p2", owner: "operator", epic: "Board" },
    blocked: { status: "draft", priority: "p2", owner: "operator", epic: "Board" },
    shipped: { status: "done", priority: "p2", owner: "operator", epic: "Board" },
    lane: { status: "doing", priority: "p2", owner: "operator", epic: "Board" }
  };

  var state = { lane: "none", tab: "overview", dialog: "", mode: "ask" };
  var prevLane = "";
  var prevDialog = "";
  var toastTimer = 0;

  function $(id) { return document.getElementById(id); }

  function resolve(hash) {
    var p = (hash || "").replace(/^#\/?/, "").split("/").filter(Boolean);
    var lane = "none", tab = "overview", dialog = "", mode = "ask";
    if (!p.length || p[0] === "overview") {
      lane = "none";
      if (p[1] && TABS.indexOf(p[1]) >= 0) tab = p[1];
    } else if (p[0] === "kickoff") {
      lane = "none";
      dialog = p[1] === "confirm" ? "confirm" : "kickoff";
    } else if (p[0] === "plan") {
      lane = "none";
      dialog = "plan";
    } else if (p[0] === "drawer") {
      lane = "drawer";
    } else if (LANES.indexOf(p[0]) >= 0) {
      lane = p[0];
      var rest = p[1] || (lane === "shipped" ? "activity" : "overview");
      if (rest === "instruction") { tab = "conversation"; mode = "instruction"; }
      else if (rest === "ask") { tab = "conversation"; mode = "ask"; }
      else if (rest === "master") { tab = "conversation"; dialog = "master"; }
      else if (DIALOGS.indexOf(rest) >= 0) {
        dialog = rest;
        tab = rest === "approve" ? "pr" : "overview";
      } else if (TABS.indexOf(rest) >= 0) {
        tab = rest;
      }
      if (p[2] === "instruction") mode = "instruction";
      else if (p[2] === "ask") mode = "ask";
      else if (p[2] && DIALOGS.indexOf(p[2]) >= 0) dialog = p[2];
    }
    if (lane === "blocked" && (dialog === "kickoff" || dialog === "confirm" || dialog === "plan")) dialog = "";
    if (dialog === "approve" && lane !== "lane" && lane !== "shipped") dialog = "";
    if (dialog === "interrupt" && lane !== "lane") dialog = "";
    if (dialog === "unfence" && lane !== "fenced") dialog = "";
    if (dialog === "reassign" && lane !== "lane" && lane !== "quota") dialog = "";
    if (dialog === "master" && lane !== "lane" && lane !== "fenced" && lane !== "quota" && lane !== "shipped") dialog = "";
    return { lane: lane, tab: tab, dialog: dialog, mode: mode };
  }

  function screenLabel(st) {
    if (st.lane === "drawer") return "Drawer peek";
    if (st.dialog === "kickoff") return "Kick off";
    if (st.dialog === "confirm") return "Kick off · confirm";
    if (st.dialog === "plan") return "Plan with master";
    if (st.dialog === "unfence") return "Unfence";
    if (st.dialog === "reassign") return "Reassign";
    if (st.dialog === "interrupt") return "Interrupt";
    if (st.dialog === "approve") return "Approve merge";
    if (st.dialog === "master") return "Ask master";
    if (st.mode === "instruction" && st.tab === "conversation") return "Instruction";
    var laneName = {
      none: "No lane", lane: "Running", fenced: "Fenced",
      quota: "Quota out", blocked: "No acceptance", shipped: "Shipped"
    }[st.lane] || st.lane;
    var tabName = {
      overview: "Overview", activity: "Activity", conversation: "Conversation",
      pr: "PR & CI", evidence: "Evidence"
    }[st.tab] || st.tab;
    return laneName + " · " + tabName;
  }

  function tabHref(lane, tab) {
    if (lane === "drawer") return "#/drawer";
    if (lane === "none") return tab === "overview" ? "#/overview" : "#/none/" + tab;
    if (lane === "lane") return tab === "overview" ? "#/lane" : "#/lane/" + tab;
    if (lane === "shipped") return tab === "activity" ? "#/shipped" : "#/shipped/" + tab;
    return tab === "overview" ? "#/" + lane : "#/" + lane + "/" + tab;
  }

  function lanePrefix(lane) {
    if (lane === "lane") return "#/lane";
    if (lane === "shipped") return "#/shipped";
    if (lane === "fenced") return "#/fenced";
    if (lane === "quota") return "#/quota";
    if (lane === "blocked") return "#/blocked";
    return "#/overview";
  }

  function toast(msg) {
    var el = $("toast");
    el.textContent = msg;
    el.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(function () { el.hidden = true; }, 4600);
  }

  function briefText() {
    var note = $("extra-note").value.trim();
    return BRIEF + "\n\n— note —\n" + (note || "No extra note.");
  }

  function bindPicker(root) {
    var buttons = root.querySelectorAll("[data-provider]");
    var model = root.querySelector("[data-model]");
    var effort = root.querySelector("[data-effort]");
    var cost = root.querySelector("[data-cost]");

    function paint() {
      var prov = root.querySelector("[data-provider].on").getAttribute("data-provider");
      var row = null;
      MODELS[prov].forEach(function (m) { if (m.id === model.value) row = m; });
      if (!row) row = MODELS[prov][0];
      cost.textContent = row.cost;
      cost.className = "badge " + (row.cost === "Paid" ? "b-warn" : row.cost === "Free (quota)" ? "b-ok" : "b-info");
    }

    function selectProv(prov, modelId, effortId) {
      buttons.forEach(function (b) {
        var on = b.getAttribute("data-provider") === prov;
        b.classList.toggle("on", on);
        b.setAttribute("aria-pressed", on ? "true" : "false");
      });
      model.innerHTML = "";
      MODELS[prov].forEach(function (m) {
        var o = document.createElement("option");
        o.value = m.id;
        o.textContent = m.id;
        model.appendChild(o);
      });
      if (modelId && MODELS[prov].some(function (m) { return m.id === modelId; })) model.value = modelId;
      effort.value = effortId || DEFAULT_EFFORT[prov];
      paint();
    }

    buttons.forEach(function (b) {
      b.addEventListener("click", function () {
        selectProv(b.getAttribute("data-provider"));
      });
    });
    model.addEventListener("change", paint);
    root.setChoice = selectProv;
    root.read = function () {
      var prov = root.querySelector("[data-provider].on").getAttribute("data-provider");
      var row = MODELS[prov][0];
      MODELS[prov].forEach(function (m) { if (m.id === model.value) row = m; });
      return {
        provider: prov,
        providerLabel: PROVIDER_LABEL[prov],
        model: model.value,
        effort: effort.value,
        cost: row.cost
      };
    };
    selectProv("cursor", "grok-4.7-high", "high");
  }

  function paintChips() {
    var status = $("f-status").value;
    var pri = $("f-priority").value;
    var epic = $("f-epic").value;
    var box = $("head-chips");
    var kind = { draft: "plain", doing: "b-info", review: "b-warn", done: "b-ok" }[status] || "plain";
    while (box.firstChild) box.removeChild(box.firstChild);
    function add(cls, text) {
      var s = document.createElement("span");
      s.className = cls;
      s.textContent = text;
      box.appendChild(s);
    }
    if (kind === "plain") add("badge plain", status);
    else add("badge " + kind, status);
    add("chip fill", pri);
    add("chip fill", epic);
  }

  function applyFields(lane) {
    var preset = FIELDS[lane] || FIELDS.lane;
    $("f-status").value = preset.status;
    $("f-priority").value = preset.priority;
    $("f-owner").value = preset.owner;
    $("f-epic").value = preset.epic;
    paintChips();
    var checks = CHECKS[lane];
    var boxes = document.querySelectorAll("#checks input");
    if (checks) {
      boxes.forEach(function (box, i) { box.checked = !!checks[i]; });
    }
    paintCount();
  }

  function paintCount() {
    var boxes = document.querySelectorAll("#checks input");
    var n = 0;
    boxes.forEach(function (b) { if (b.checked) n += 1; });
    var el = $("accept-count");
    if (el) el.textContent = n + " of " + boxes.length;
  }

  function setAction(btn, spec) {
    btn.disabled = !spec.enabled;
    btn.className = "btn" + (spec.primary ? " btn-acc" : "");
    btn.textContent = spec.label;
    btn.title = spec.title || "";
    if (spec.go) btn.setAttribute("data-go", spec.go);
    else btn.removeAttribute("data-go");
  }

  function paintActions(lane) {
    var kick = $("act-kick"), ask = $("act-ask"), approve = $("act-approve");
    if (lane === "none") {
      setAction(kick, { enabled: true, primary: true, label: "Kick off", go: "#/kickoff" });
      setAction(ask, { enabled: false, label: "Ask agent", title: "No lane yet" });
      setAction(approve, { enabled: false, label: "Approve", title: "No pull request yet" });
    } else if (lane === "blocked") {
      setAction(kick, { enabled: false, label: "Kick off", title: "Kick off needs at least one acceptance check." });
      setAction(ask, { enabled: false, label: "Ask agent", title: "No lane yet" });
      setAction(approve, { enabled: false, label: "Approve", title: "No pull request yet" });
    } else if (lane === "lane") {
      setAction(kick, { enabled: false, label: "Kick off", title: "A lane is already running" });
      setAction(ask, { enabled: true, primary: true, label: "Ask agent", go: "#/lane/conversation" });
      setAction(approve, { enabled: true, label: "Approve", go: "#/lane/approve" });
    } else if (lane === "fenced") {
      setAction(kick, { enabled: false, label: "Kick off", title: "Agent is fenced" });
      setAction(ask, { enabled: false, label: "Ask agent", title: "Agent is fenced" });
      setAction(approve, { enabled: false, label: "Approve", title: "No merge while the lane is fenced" });
    } else if (lane === "quota") {
      setAction(kick, { enabled: false, label: "Kick off", title: "Quota is out — reassign instead" });
      setAction(ask, { enabled: false, label: "Ask agent", title: "Agent is quota-out" });
      setAction(approve, { enabled: false, label: "Approve", title: "No merge while the lane is quota-out" });
    } else if (lane === "shipped") {
      setAction(kick, { enabled: false, label: "Kick off", title: "Already shipped" });
      setAction(ask, { enabled: false, label: "Ask agent", title: "Lane is closed" });
      setAction(approve, { enabled: false, label: "Approved", title: "Human-class approval is recorded" });
    }
  }

  function paintSummary() {
    var pick = $("kick-picker").read();
    var pm = $("pm-group").value;
    $("sum-who").textContent = pick.providerLabel + " · " + pick.model + " · " + pick.effort;
    $("sum-cost").textContent = pick.cost;
    $("sum-cost").className = "badge " + (pick.cost === "Paid" ? "b-warn" : pick.cost === "Free (quota)" ? "b-ok" : "b-info");
    $("sum-pm").textContent = pm;
    $("sum-brief").textContent = briefText();
    $("paid-warn").hidden = pick.cost !== "Paid";
  }

  function paintComposer(st) {
    var ask = $("mode-ask"), inst = $("mode-instruction");
    if (!ask) return;
    var instruction = st.mode === "instruction";
    ask.classList.toggle("on", !instruction);
    inst.classList.toggle("on", instruction);
    ask.setAttribute("aria-pressed", instruction ? "false" : "true");
    inst.setAttribute("aria-pressed", instruction ? "true" : "false");
    $("composer-text").placeholder = instruction
      ? "Instruction for the lane. This is a real message."
      : "Ask what the lane is doing. Light nudge, one turn.";
    $("composer-hint").textContent = instruction
      ? "A real message to the lane. Steer amendments come later."
      : "Light nudge. One turn, then the answer lands in this thread.";
    $("do-send").textContent = instruction ? "Send instruction" : "Ask";
  }

  function render() {
    state = resolve(location.hash);
    var label = screenLabel(state);
    $("snap").textContent = label;
    document.title = label + " — CAD-604 · cadence mockup";

    var drawer = state.lane === "drawer";
    $("app").hidden = drawer;
    $("drawer-wrap").hidden = !drawer;
    $("crumb-leaf").textContent = drawer ? "board" : "CAD-604";
    var issues = document.querySelector('[data-navlink="issues"]');
    if (issues) {
      if (drawer) issues.removeAttribute("aria-current");
      else issues.setAttribute("aria-current", "page");
    }

    document.querySelectorAll("[data-panel]").forEach(function (el) {
      el.hidden = el.getAttribute("data-panel") !== state.tab;
    });
    document.querySelectorAll("[data-show]").forEach(function (el) {
      var ok = el.getAttribute("data-show").split(/\s+/).indexOf(state.lane) >= 0;
      el.hidden = !ok;
    });
    var banners = $("banners");
    var anyBanner = false;
    banners.querySelectorAll("[data-show]").forEach(function (el) { if (!el.hidden) anyBanner = true; });
    banners.hidden = !anyBanner;

    document.querySelectorAll("[data-dialog]").forEach(function (el) {
      el.hidden = el.getAttribute("data-dialog") !== state.dialog || state.dialog === "master";
    });
    var master = $("master-card");
    if (master) master.hidden = state.dialog !== "master";

    TABS.forEach(function (tab) {
      var a = document.querySelector('#tabs a[data-tab="' + tab + '"]');
      if (!a) return;
      a.setAttribute("href", tabHref(state.lane, tab));
      var on = !drawer && state.tab === tab;
      a.classList.toggle("on", on);
      if (on) a.setAttribute("aria-current", "page");
      else a.removeAttribute("aria-current");
    });

    var cur = [state.lane, state.tab, state.dialog, state.mode].filter(Boolean);
    var best = null, bestLen = -1;
    document.querySelectorAll("#screen-nav a").forEach(function (a) {
      var tokens = (a.getAttribute("data-nav") || "").split(/\s+/).filter(Boolean);
      var ok = tokens.every(function (t) { return cur.indexOf(t) >= 0; });
      if (ok && tokens.length > bestLen) { best = a; bestLen = tokens.length; }
      a.removeAttribute("aria-current");
    });
    if (best) best.setAttribute("aria-current", "page");

    paintActions(state.lane);
    paintComposer(state);
    if (state.lane !== prevLane && state.lane !== "drawer") applyFields(state.lane);
    if (state.dialog === "confirm") paintSummary();
    if (state.dialog === "reassign" && prevDialog !== "reassign") {
      var lead = $("reassign-lead");
      if (state.lane === "quota") {
        lead.textContent = "swe-2-high is quota-out and the last turn was rate-limited. Reassign off Devin. The worktree stays.";
        $("re-picker").setChoice("cursor", "grok-4.7-high", "high");
      } else {
        lead.textContent = "Move lane-1 to another provider or model. The worktree stays.";
        $("re-picker").setChoice("devin", "swe-2-high", "max");
      }
    }
    $("brief-preview").textContent = briefText();
    $("reassign-cancel").setAttribute("data-go", state.lane === "quota" ? "#/quota" : "#/lane");
    $("master-close").setAttribute("data-go", lanePrefix(state.lane) + "/conversation");
    $("tl-pr-link").setAttribute("href", tabHref(state.lane, "pr"));
    if (state.dialog && state.dialog !== prevDialog && state.dialog !== "master") {
      var dlg = document.querySelector('[data-dialog="' + state.dialog + '"]');
      var focus = dlg && dlg.querySelector("button, [href], input, select, textarea");
      if (focus) focus.focus();
    }
    prevLane = state.lane;
    prevDialog = state.dialog;
    document.documentElement.classList.add("ready");
  }

  function closeDialog() {
    if (!state.dialog) return;
    if (state.dialog === "reassign") location.hash = state.lane === "quota" ? "#/quota" : "#/lane";
    else if (state.dialog === "approve") location.hash = state.lane === "shipped" ? "#/shipped/pr" : "#/lane/pr";
    else if (state.dialog === "master") location.hash = lanePrefix(state.lane) + "/conversation";
    else if (state.dialog === "confirm") location.hash = "#/kickoff";
    else if (state.dialog === "kickoff" || state.dialog === "plan") location.hash = "#/overview";
    else if (state.dialog === "unfence") location.hash = "#/fenced";
    else if (state.dialog === "interrupt") location.hash = "#/lane";
  }

  function appendMessage(mode, text) {
    var wrap = $("thread-lane");
    var art = document.createElement("article");
    art.className = "msg op";
    var head = document.createElement("header");
    head.textContent = "you · just now";
    var badge = document.createElement("span");
    badge.className = "badge plain";
    badge.textContent = mode === "instruction" ? "instruction" : "status";
    head.appendChild(badge);
    var p = document.createElement("p");
    p.textContent = text;
    art.appendChild(head);
    art.appendChild(p);
    wrap.appendChild(art);
  }

  var kick = $("kick-picker");
  var re = $("re-picker");
  bindPicker(kick);
  bindPicker(re);

  $("extra-note").addEventListener("input", function () {
    $("brief-preview").textContent = briefText();
    if (state.dialog === "confirm") paintSummary();
  });
  $("pm-group").addEventListener("change", function () {
    if (state.dialog === "confirm") paintSummary();
  });

  ["f-status", "f-priority", "f-owner", "f-epic"].forEach(function (id) {
    $(id).addEventListener("change", paintChips);
  });
  document.querySelectorAll("#checks input").forEach(function (box) {
    box.addEventListener("change", paintCount);
  });

  document.body.addEventListener("click", function (e) {
    var toastBtn = e.target.closest("[data-toast]");
    if (toastBtn) {
      toast(toastBtn.getAttribute("data-toast"));
      e.preventDefault();
      return;
    }
    var go = e.target.closest("[data-go]");
    if (go && go.tagName !== "A") {
      if (go.disabled) return;
      location.hash = go.getAttribute("data-go");
    }
  });

  $("mode-ask").addEventListener("click", function () {
    location.hash = lanePrefix(state.lane) + "/conversation";
  });
  $("mode-instruction").addEventListener("click", function () {
    location.hash = lanePrefix(state.lane) + "/instruction";
  });
  $("ask-master").addEventListener("click", function () {
    location.hash = lanePrefix(state.lane) + "/master";
  });

  $("do-send").addEventListener("click", function () {
    var text = $("composer-text").value.trim();
    if (!text) { toast("Write a message first."); return; }
    var mode = state.mode === "instruction" ? "instruction" : "ask";
    appendMessage(mode, text);
    $("composer-text").value = "";
    toast(mode === "instruction"
      ? "Would send an instruction to lane-1. Shown here only."
      : "Would send a status nudge to lane-1. The answer would land in this thread.");
  });
  $("do-master").addEventListener("click", function () {
    var text = $("master-draft").value.trim();
    if (!text) { toast("Write a note to master first."); return; }
    toast("Would send this to master. The lane was not messaged.");
  });

  $("do-kick").addEventListener("click", function () {
    paintSummary();
    var pick = kick.read();
    toast("Would run issue start, then dispatch (" + pick.providerLabel + " · " + pick.cost + "). Nothing was sent.");
  });
  $("do-plan").addEventListener("click", function () {
    toast("Would ask master to staff the lane after this approval. Nothing started.");
  });
  $("do-reassign").addEventListener("click", function () {
    var pick = re.read();
    toast("Would reassign lane-1 to " + pick.providerLabel + " · " + pick.model + " (" + pick.cost + ").");
  });
  $("do-interrupt").addEventListener("click", function () {
    toast("Would interrupt the current turn. The lane stays claimed.");
    location.hash = "#/lane";
  });
  $("do-stop").addEventListener("click", function () {
    toast("Would stop lane-1. Queued messages would stay.");
    location.hash = "#/lane";
  });
  $("do-approve").addEventListener("click", function () {
    toast("Would record a human-class operator approval on PR #900. The queue was not released.");
  });

  document.querySelectorAll('input[name="unfence-status"]').forEach(function (r) {
    r.addEventListener("change", function () { $("do-unfence").disabled = false; });
  });
  $("do-unfence").addEventListener("click", function () {
    var picked = document.querySelector('input[name="unfence-status"]:checked');
    if (!picked) { toast("Choose a status first."); return; }
    var resume = $("unfence-resume").checked ? "Resume after." : "Leave stopped.";
    toast("Would unfence lane-1 as " + picked.value + ". " + resume + " The turn would not be replayed.");
  });

  document.querySelectorAll(".scrim").forEach(function (scrim) {
    scrim.addEventListener("click", function (e) {
      if (e.target === scrim) closeDialog();
    });
  });

  addEventListener("hashchange", render);
  addEventListener("keydown", function (e) {
    if (e.key === "Escape" && state.dialog) {
      e.preventDefault();
      closeDialog();
    }
  });
  render();

  /* Theme toggle — same cycle as kit.html: system → light → dark → system. */
  var KEY = "cadence-theme";
  var icons = {
    system: '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4"><circle cx="8" cy="8" r="5.5"/><path d="M8 2.5v11a5.5 5.5 0 0 0 0-11z" fill="currentColor"/></svg>',
    light: '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4"><circle cx="8" cy="8" r="3"/><path d="M8 1.5v1.8M8 12.7v1.8M1.5 8h1.8M12.7 8h1.8M3.4 3.4l1.3 1.3M11.3 11.3l1.3 1.3M12.6 3.4l-1.3 1.3M4.7 11.3l-1.3 1.3"/></svg>',
    dark: '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4"><path d="M13.5 9.6A5.8 5.8 0 0 1 6.4 2.5a5.8 5.8 0 1 0 7.1 7.1z"/></svg>'
  };
  function pref() {
    try { var v = localStorage.getItem(KEY); return v === "light" || v === "dark" ? v : "system"; }
    catch (err) { return "system"; }
  }
  var next = { system: "light", light: "dark", dark: "system" };
  var themeBtn = $("theme-toggle");
  function paintTheme(p) {
    themeBtn.innerHTML = icons[p];
    themeBtn.title = "theme: " + p + " — switch to " + next[p];
    themeBtn.setAttribute("aria-label", themeBtn.title);
  }
  paintTheme(pref());
  themeBtn.addEventListener("click", function () {
    var p = next[pref()];
    try { if (p === "system") localStorage.removeItem(KEY); else localStorage.setItem(KEY, p); }
    catch (err) { /* keep the in-memory theme */ }
    if (p === "system") document.documentElement.removeAttribute("data-theme");
    else document.documentElement.setAttribute("data-theme", p);
    paintTheme(p);
  });
})();
