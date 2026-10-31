/* Approval card states (CAD-1946). Synthetic data only; nothing here calls a
   real endpoint. */

const ROWS = [
  { id: "reviews", text: "Two independent reviews" },
  { id: "browser", text: "Tested in the browser" },
  { id: "checks", text: "Checks passed" },
  { id: "approval", text: "Your approval" },
];

const STATES = {
  ready: {
    sub: "Ready to merge",
    badge: ["b-ok", "Ready"],
    rows: { reviews: "ok", browser: "ok", checks: "ok", approval: "ok" },
    hint: { reviews: "2 pass · 290710c", browser: "Safari, Chrome", checks: "run 41186770970" },
    banner: null,
    action: "approve",
  },
  "needs-one": {
    sub: "Needs 1 more thing",
    badge: ["b-warn", "Not yet"],
    rows: { reviews: "ok", browser: "todo", checks: "ok", approval: "ok" },
    hint: { reviews: "2 pass · 290710c", browser: "missing", checks: "run 41186770970" },
    banner: ["warn", "One thing is still missing: this change touches the UI and has not been tested in a browser."],
    action: null,
  },
  "head-moved": {
    sub: "Needs a fresh review",
    badge: ["b-warn", "Stale"],
    rows: { reviews: "blocked", browser: "blocked", checks: "waiting", approval: "blocked" },
    hint: { reviews: "voided", browser: "voided", checks: "—" },
    banner: ["warn", "A newer version came in — it needs a fresh review. The reviews and the approval you gave were for the previous version."],
    action: "send-back",
  },
  ejected: {
    sub: "Didn't merge",
    badge: ["b-fail", "Ejected"],
    rows: { reviews: "ok", browser: "ok", checks: "blocked", approval: "ok" },
    hint: { reviews: "2 pass", browser: "Safari, Chrome", checks: "failed" },
    banner: ["fail", "Didn't merge — a check failed. Nothing was merged."],
    link: ["Open the failed run", "https://example.invalid/run/41186770970"],
    action: "retry",
  },
  down: {
    sub: "Can't check right now",
    badge: ["b-info", "Unknown"],
    rows: { reviews: "waiting", browser: "waiting", checks: "waiting", approval: "waiting" },
    hint: {},
    banner: ["warn", "Can't check right now — nothing was approved. The board could not reach the workspace, so every item is unknown rather than green."],
    action: "retry",
  },
  "step-up": {
    sub: "Ready to merge",
    badge: ["b-ok", "Ready"],
    rows: { reviews: "ok", browser: "ok", checks: "ok", approval: "ok" },
    hint: { reviews: "2 pass", browser: "Safari, Chrome", checks: "run 41186770970" },
    banner: null,
    action: "stepup",
  },
  merging: {
    sub: "Approved — merging",
    badge: ["b-info", "Merging"],
    rows: { reviews: "ok", browser: "ok", checks: "ok", approval: "ok" },
    hint: { reviews: "2 pass", browser: "Safari, Chrome", checks: "queued" },
    banner: null,
    action: "revoke",
    progress: ["approved", "queued", "checks", "merged"],
    progressAt: 1,
  },
};

const stage = document.getElementById("stage");
const rowsEl = document.getElementById("rows");
const subEl = document.getElementById("card-sub");
const badgeEl = document.getElementById("card-badge");
const bannerSlot = document.getElementById("banner-slot");
const actionsSlot = document.getElementById("actions-slot");
const live = document.getElementById("live");
const picker = document.getElementById("state");

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function renderDetails() {
  const rows = ROWS.map((row) => {
    const details = el("details", "row");
    details.dataset.state = "waiting";
    details.dataset.id = row.id;
    const summary = el("summary");
    summary.append(el("span", "dot"), el("span", "row-text", row.text));
    const hint = el("span", "row-hint");
    hint.dataset.hint = row.id;
    summary.append(hint);
    details.append(summary);
    const body = el("div", "row-body");
    body.dataset.body = row.id;
    details.append(body);
    return details;
  });
  rowsEl.replaceChildren(...rows);
}

const BODY = {
  reviews: "Two reviewers read this exact version and both passed it. Neither wrote the change.",
  browser: "Someone opened the changed screens and used them, rather than only running tests.",
  checks: "The required checks ran on this version and passed.",
  approval: "Yours only. Members and agents cannot approve, and an approval is for one version.",
};

function render(stateKey) {
  const state = STATES[stateKey];
  subEl.textContent = state.sub;
  badgeEl.className = `badge ${state.badge[0]}`;
  badgeEl.textContent = state.badge[1];

  for (const row of ROWS) {
    const details = rowsEl.querySelector(`[data-id="${row.id}"]`);
    const status = state.rows[row.id] ?? "waiting";
    details.dataset.state = status;
    const hint = details.querySelector(`[data-hint="${row.id}"]`);
    hint.textContent = state.hint[row.id] ?? "";
    const body = details.querySelector(`[data-body="${row.id}"]`);
    body.textContent = BODY[row.id];
  }

  bannerSlot.replaceChildren();
  if (state.banner) {
    const [tone, text] = state.banner;
    const banner = el("div", "banner", text);
    banner.dataset.tone = tone;
    if (state.link) {
      const a = el("a", null, state.link[0]);
      a.href = state.link[1];
      a.rel = "noreferrer";
      banner.append(" ");
      banner.append(a);
    }
    bannerSlot.append(banner);
  }

  actionsSlot.replaceChildren();
  const grow = el("span", "grow");
  actionsSlot.append(grow);
  const cancel = el("button", "btn btn-ghost", "Close");
  cancel.type = "button";
  actionsSlot.append(cancel);

  if (state.progress) {
    const list = el("ol", "progress");
    state.progress.forEach((step, index) => {
      const item = el("li", null, step);
      item.dataset.step = index < state.progressAt ? "done" : "todo";
      list.append(item);
    });
    bannerSlot.append(list);
  }

  if (state.action === "approve") {
    const approve = el("button", "btn btn-primary", "Approve and merge");
    approve.type = "button";
    approve.addEventListener("click", () => setState("step-up"));
    actionsSlot.append(approve);
  }
  if (state.action === "stepup") {
    const panel = el("div", "stepup");
    panel.append(el("p", null, "Confirm it's you. The board will not take an approval from an older sign-in."));
    const confirm = el("button", "btn btn-primary", "Confirm with passkey");
    confirm.type = "button";
    confirm.addEventListener("click", () => setState("merging"));
    panel.append(confirm);
    actionsSlot.before(panel);
  }
  if (state.action === "revoke") {
    const revoke = el("button", "btn btn-ghost", "Take my approval back");
    revoke.type = "button";
    revoke.addEventListener("click", () => setState("ready"));
    actionsSlot.append(revoke);
  }
  if (state.action === "retry") {
    const retry = el("button", "btn btn-sm", "Check again");
    retry.type = "button";
    retry.addEventListener("click", () => setState("ready"));
    actionsSlot.append(retry);
  }
  if (state.action === "send-back") {
    const send = el("button", "btn btn-ghost", "Send back with a reason");
    send.type = "button";
    actionsSlot.append(send);
  }

  live.textContent = `${state.sub}. ${state.banner ? state.banner[1] : ""}`;
  picker.value = stateKey;
  try {
    // Keep whatever else is in the query — the preview token lives there, and
    // dropping it would lock the reviewer out of the page they are reading.
    const params = new URLSearchParams(location.search);
    params.set("state", stateKey);
    history.replaceState(null, "", `${location.pathname}?${params}`);
  } catch (e) { /* file:// without history is fine */ }
}

function setState(key) {
  render(key);
}

document.querySelectorAll("[data-theme-pick]").forEach((button) => {
  button.addEventListener("click", () => {
    const pick = button.dataset.themePick;
    if (pick) document.documentElement.setAttribute("data-theme", pick);
    else document.documentElement.removeAttribute("data-theme");
    try {
      localStorage.setItem("cadence-theme", pick || "");
    } catch (e) { /* ignore */ }
    syncThemeButtons();
  });
});

function syncThemeButtons() {
  const current = document.documentElement.getAttribute("data-theme") || "";
  document.querySelectorAll("[data-theme-pick]").forEach((button) => {
    button.setAttribute("aria-pressed", String(button.dataset.themePick === current));
  });
}

document.querySelectorAll("[data-width-pick]").forEach((button) => {
  button.addEventListener("click", () => {
    stage.dataset.width = button.dataset.widthPick;
    syncWidthButtons();
  });
});

function syncWidthButtons() {
  document.querySelectorAll("[data-width-pick]").forEach((button) => {
    button.setAttribute("aria-pressed", String(button.dataset.widthPick === stage.dataset.width));
  });
}

picker.addEventListener("change", () => setState(picker.value));

renderDetails();
syncThemeButtons();
syncWidthButtons();
const initial = new URLSearchParams(location.search).get("state");
setState(STATES[initial] ? initial : "ready");
