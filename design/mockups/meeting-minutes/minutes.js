/* ASR writes 8-second batches. GLM 5.3 Flash consolidates those lines. */
(function () {
  "use strict";

  var BATCHES = [
    ["00:00", "Launch moves to Thursday. The checklist is the remaining work."],
    ["00:08", "Design owns the checklist. Ops will confirm the vendor by Wednesday."],
    ["00:16", "No other blockers. We'll read the minutes after this."]
  ];
  var MINUTES = [
    null,
    {
      summary: "Launch moves to Thursday. The checklist is still open.",
      decisions: ["Launch moves to Thursday."],
      followups: [["Design", "Finish the checklist."]]
    },
    {
      summary: "Launch is Thursday. Design owns the checklist, and Ops will confirm the vendor by Wednesday.",
      decisions: ["Launch moves to Thursday.", "Design owns the checklist."],
      followups: [["Design", "Finish the checklist."], ["Ops", "Confirm the vendor by Wednesday."]]
    },
    {
      summary: "Launch is Thursday. Design owns the checklist, Ops confirms the vendor by Wednesday, and nothing else is blocking.",
      decisions: ["Launch moves to Thursday.", "No other blockers."],
      followups: [["Design", "Finish the checklist."], ["Ops", "Confirm the vendor by Wednesday."]]
    }
  ];
  var READY = "mm-transcript-ready";

  var views = Array.prototype.slice.call(document.querySelectorAll("[data-view]"));
  var crumb = document.getElementById("crumb");
  var clock = document.getElementById("rec-time");
  var deck = document.getElementById("deck");
  var label = document.getElementById("rec-label");
  var note = document.getElementById("rec-note");
  var titleInput = document.getElementById("rec-title");
  var toggle = document.getElementById("toggle");
  var stopBtn = document.getElementById("stop");
  var liveLines = document.getElementById("live-lines");
  var liveEmpty = document.getElementById("live-empty");
  var uploaded = document.getElementById("uploaded");
  var batches = document.getElementById("batches");
  var status = document.getElementById("process-status");
  var transcript = document.getElementById("new-transcript");
  var newRow = document.getElementById("new-row");
  var toast = document.getElementById("toast");
  var tick = 0;
  var step = 0;
  var timer = 0;
  var phase = "ready";
  var elapsedMs = 0;
  var runningSince = 0;
  var shown = 0;
  var captured = [];
  var GENERATED_TITLE = "Thursday launch";
  var meetingTitle = GENERATED_TITLE;
  var titleFromUser = false;
  var people = [];
  var titles = {
    list: "Meeting minutes",
    record: "New meeting",
    process: "Consolidating",
    catalog: "App catalog",
    sprint: "Sprint plan",
    new: "Meeting"
  };

  function ready() {
    try { return sessionStorage.getItem(READY) === "1"; }
    catch (e) { return false; }
  }

  function markReady() {
    try { sessionStorage.setItem(READY, "1"); } catch (e) {}
    newRow.hidden = false;
  }

  function route() {
    var h = location.hash.replace(/^#\/?/, "");
    if (h === "record") return "record";
    if (h === "process") return "process";
    if (h === "m/catalog") return "catalog";
    if (h === "m/sprint") return "sprint";
    if (h === "m/new") return ready() ? "new" : "list";
    return "list";
  }

  function pad(n) { return n < 10 ? "0" + n : String(n); }

  function line(stamp, text) {
    var li = document.createElement("li");
    var time = document.createElement("span");
    time.className = "cite";
    time.textContent = stamp;
    var body = document.createElement("span");
    body.textContent = text;
    li.appendChild(time);
    li.appendChild(body);
    return li;
  }

  function chipLine(chip, text) {
    var li = document.createElement("li");
    var mark = document.createElement("span");
    mark.className = chip === "decision" ? "badge b-info" : "badge plain";
    mark.textContent = chip;
    var body = document.createElement("span");
    body.textContent = text;
    li.appendChild(mark);
    li.appendChild(body);
    return li;
  }

  function elapsed() {
    return elapsedMs + (phase === "recording" ? Date.now() - runningSince : 0);
  }

  function paintClock() {
    var s = Math.floor(elapsed() / 1000);
    clock.textContent = pad(Math.floor(s / 60)) + ":" + pad(s % 60);
  }

  function paintLive() {
    var rows = BATCHES.slice(0, shown);
    liveLines.replaceChildren.apply(liveLines, rows.map(function (row) {
      return line(row[0], row[1]);
    }));
    liveLines.hidden = shown === 0;
    liveEmpty.hidden = shown !== 0;
    uploaded.textContent = shown === 0 ? "Waiting" : shown + (shown === 1 ? " batch" : " batches");
  }

  function catchUp() {
    var due = Math.min(BATCHES.length, Math.floor(elapsed() / 8000));
    if (due === shown) return;
    shown = due;
    paintLive();
  }

  function paintPhase() {
    deck.setAttribute("data-state", phase);
    stopBtn.disabled = phase === "ready";
    if (phase === "ready") {
      label.textContent = "Ready";
      toggle.textContent = "Record";
      note.textContent = "Press record when the room is ready. Speech is written to text every 8 seconds. Stopping consolidates those lines into minutes.";
    } else if (phase === "recording") {
      label.textContent = "Recording";
      toggle.textContent = "Pause";
      note.textContent = "Pause if you step out. Stop consolidates only the lines already written.";
    } else {
      label.textContent = "Paused";
      toggle.textContent = "Resume";
      note.textContent = "Nothing new is captured while paused. Resume, or stop and write the minutes.";
    }
  }

  function resetRecorder() {
    clearInterval(tick);
    phase = "ready";
    elapsedMs = 0;
    runningSince = 0;
    shown = 0;
    titleInput.value = "";
    document.getElementById("rec-people").value = "";
    paintClock();
    paintLive();
    paintPhase();
  }

  function armClock() {
    clearInterval(tick);
    tick = setInterval(function () {
      paintClock();
      catchUp();
    }, 250);
  }

  function fillMinutes() {
    var pack = MINUTES[captured.length] || MINUTES[1];
    document.getElementById("new-summary").textContent = pack.summary;
    document.getElementById("new-decisions").replaceChildren.apply(
      document.getElementById("new-decisions"),
      pack.decisions.map(function (text) { return chipLine("decision", text); })
    );
    document.getElementById("new-followups").replaceChildren.apply(
      document.getElementById("new-followups"),
      pack.followups.map(function (row) { return chipLine(row[0], row[1]); })
    );
    transcript.replaceChildren.apply(transcript, captured.map(function (row) {
      return line(row[0], row[1]);
    }));
    document.getElementById("new-kicker").textContent = clock.textContent || "Just now";
    document.getElementById("new-title").textContent = meetingTitle;
    var origin = document.getElementById("title-origin");
    origin.textContent = titleFromUser ? "your title" : "from the discussion";
    origin.className = titleFromUser ? "badge plain" : "badge b-info";
    document.getElementById("title-note").textContent = titleFromUser
      ? "You named this meeting. That name is kept."
      : "Left blank, so this name came from the discussion. Rename it if you want a different one.";
    var peopleRow = document.getElementById("new-people");
    peopleRow.replaceChildren.apply(peopleRow, people.map(function (name) {
      var chip = document.createElement("span");
      chip.className = "chip";
      chip.textContent = name;
      return chip;
    }));
    peopleRow.hidden = people.length === 0;
    document.getElementById("rename-form").hidden = true;
  }

  function beginProcess() {
    if (!captured.length) {
      location.hash = "#/record";
      return;
    }
    clearTimeout(step);
    document.getElementById("process-title").textContent = titleFromUser ? meetingTitle : "Consolidating";
    status.textContent = (titleFromUser
      ? "Your title stays. GLM 5.3 Flash is consolidating "
      : "GLM 5.3 Flash will name it from the discussion, and is consolidating ")
      + captured.length + (captured.length === 1 ? " line." : " lines.");
    batches.replaceChildren.apply(batches, captured.map(function (row) {
      return line(row[0], row[1]);
    }));
    var reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    step = setTimeout(function () {
      if (route() !== "process") return;
      markReady();
      var when = document.getElementById("new-when");
      when.textContent = clock.textContent || "Just now";
      newRow.querySelector(".rowlink").textContent = meetingTitle;
      var originMark = document.getElementById("new-origin");
      originMark.textContent = titleFromUser ? "your title" : "from the discussion";
      originMark.className = titleFromUser ? "badge plain" : "badge b-info";
      location.hash = "#/m/new";
    }, reduce ? 0 : 900);
  }

  function render() {
    var name = route();
    if (location.hash.replace(/^#\/?/, "") === "m/new" && !ready()) location.hash = "#/";
    views.forEach(function (s) {
      s.hidden = s.getAttribute("data-view") !== name;
    });
    crumb.textContent = name === "new" ? meetingTitle : titles[name];
    if (name !== "record") clearInterval(tick);
    if (name === "record") {
      if (phase === "recording") armClock();
      else if (phase !== "paused") resetRecorder();
    }
    if (name === "process") beginProcess();
    if (name === "new") fillMinutes();
    if (ready()) newRow.hidden = false;
    window.scrollTo(0, 0);
  }

  function ping(text) {
    toast.textContent = text;
    toast.hidden = false;
    clearTimeout(timer);
    timer = setTimeout(function () { toast.hidden = true; }, 2800);
  }

  if (ready()) newRow.hidden = false;
  addEventListener("hashchange", render);
  if (!location.hash) location.hash = "#/";
  else render();

  document.addEventListener("click", function (e) {
    var row = e.target.closest("tr[data-goto]");
    if (!row || e.target.closest("a, button")) return;
    if (row.id === "new-row" && !ready()) return;
    location.hash = row.getAttribute("data-goto");
  });

  toggle.addEventListener("click", function () {
    if (phase === "recording") {
      elapsedMs = elapsed();
      phase = "paused";
      clearInterval(tick);
      paintClock();
      paintPhase();
      return;
    }
    phase = "recording";
    runningSince = Date.now();
    paintPhase();
    armClock();
  });

  stopBtn.addEventListener("click", function () {
    if (phase === "recording") {
      elapsedMs = elapsed();
      clearInterval(tick);
    }
    catchUp();
    if (!shown) {
      ping("Keep going until the first line appears, then stop.");
      if (phase === "paused") {
        phase = "recording";
        runningSince = Date.now();
        paintPhase();
        armClock();
      }
      return;
    }
    captured = BATCHES.slice(0, shown);
    var typed = titleInput.value.trim();
    titleFromUser = typed.length > 0;
    meetingTitle = titleFromUser ? typed : GENERATED_TITLE;
    people = document.getElementById("rec-people").value.split(",").map(function (name) {
      return name.trim();
    }).filter(Boolean);
    phase = "ready";
    clearInterval(tick);
    location.hash = "#/process";
  });

  function applyRename(next) {
    meetingTitle = next;
    titleFromUser = true;
    newRow.querySelector(".rowlink").textContent = next;
    var originMark = document.getElementById("new-origin");
    originMark.textContent = "your title";
    originMark.className = "badge plain";
    fillMinutes();
    crumb.textContent = next;
  }

  document.getElementById("rename").addEventListener("click", function () {
    var form = document.getElementById("rename-form");
    document.getElementById("rename-input").value = meetingTitle;
    form.hidden = false;
    document.getElementById("rename-input").focus();
  });

  document.getElementById("rename-cancel").addEventListener("click", function () {
    document.getElementById("rename-form").hidden = true;
  });

  document.getElementById("rename-form").addEventListener("submit", function (e) {
    e.preventDefault();
    var next = document.getElementById("rename-input").value.trim();
    if (!next) return;
    applyRename(next);
  });

  document.getElementById("composer").addEventListener("submit", function (e) {
    e.preventDefault();
    var q = String(new FormData(e.target).get("q") || "").trim().toLowerCase();
    e.target.reset();
    if (!q) return;
    if (q.indexOf("record") !== -1) location.hash = "#/record";
    else if (q.indexOf("latest") !== -1 || q.indexOf("catalog") !== -1) location.hash = "#/m/catalog";
    else if (q.indexOf("sprint") !== -1) location.hash = "#/m/sprint";
    else ping("Record a meeting, or open one from the list.");
  });
})();
