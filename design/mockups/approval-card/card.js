/* Approval card states (CAD-1946). Synthetic data; no endpoint is called. */

const ROWS = [
  { id: "reviews", text: "Two independent reviews" },
  { id: "browser", text: "Tested in the browser" },
  { id: "checks", text: "Checks passed" },
  { id: "approval", text: "Your approval" },
];

const BODY = {
  reviews: "Two reviewers read this exact version and both passed it. Neither wrote the change.",
  browser: "Someone opened the changed screens and used them, rather than only running tests.",
  checks: "The required checks ran on this version and passed.",
  approval: "Yours only. Members and agents cannot approve, and an approval is for one version.",
};

const WORDS = { ok: "pass", todo: "missing", blocked: "failed", waiting: "unknown" };

const SUMMARY = "4 files · +412 −37 · touches the release workflow";

const STATES = {
  ready: {
    sub: "Ready to merge",
    summary: SUMMARY,
    rows: { reviews: "ok", browser: "ok", checks: "ok", approval: "waiting" },
    hint: { reviews: "290710c", browser: "Safari, Chrome", checks: "run 41186770970" },
    banner: null,
    action: "approve",
  },
  "needs-one": {
    sub: "Missing: tested in the browser",
    summary: SUMMARY,
    rows: { reviews: "ok", browser: "todo", checks: "ok", approval: "waiting" },
    hint: { reviews: "290710c", browser: "", checks: "run 41186770970" },
    banner: ["warn", "This change touches the UI and has not been tested in a browser yet."],
    action: null,
  },
  "head-moved": {
    sub: "Needs a fresh review",
    summary: "290710c → 41b7e02 · the version changed after review",
    rows: { reviews: "blocked", browser: "blocked", checks: "waiting", approval: "blocked" },
    hint: { reviews: "voided", browser: "voided", checks: "" },
    banner: [
      "warn",
      "A newer version came in. The reviews and the approval you gave were for the previous version.",
    ],
    action: "send-back",
  },
  ejected: {
    sub: "Didn't merge",
    summary: SUMMARY,
    rows: { reviews: "ok", browser: "ok", checks: "blocked", approval: "ok" },
    hint: { reviews: "290710c", browser: "Safari, Chrome", checks: "failed" },
    banner: ["fail", "Didn't merge — a check failed. Nothing was merged."],
    link: ["Open the failed run", "https://example.invalid/run/41186770970"],
    action: "retry",
  },
  down: {
    sub: "Can't check right now",
    summary: "4 files · nothing could be checked",
    rows: { reviews: "waiting", browser: "waiting", checks: "waiting", approval: "waiting" },
    hint: {},
    banner: [
      "warn",
      "The board could not reach the workspace, so every item is unknown rather than green. Nothing was approved.",
    ],
    action: "retry",
  },
  "step-up": {
    sub: "Ready to merge",
    summary: SUMMARY,
    rows: { reviews: "ok", browser: "ok", checks: "ok", approval: "waiting" },
    hint: { reviews: "290710c", browser: "Safari, Chrome", checks: "run 41186770970" },
    banner: null,
    action: "stepup",
  },
  merging: {
    sub: "Approved — merging",
    summary: "approved by you for 290710c",
    rows: { reviews: "ok", browser: "ok", checks: "ok", approval: "ok" },
    hint: { reviews: "290710c", browser: "Safari, Chrome", checks: "queued" },
    banner: null,
    action: "revoke",
    progress: ["approved", "queued", "checks", "merged"],
    progressAt: 1,
  },
};

const sheet = document.getElementById("sheet");
const rowsEl = document.getElementById("rows");
const subEl = document.getElementById("card-sub");
const summaryEl = document.getElementById("card-summary");
const bannerSlot = document.getElementById("banner-slot");
const actionSlot = document.getElementById("action-slot");
const live = document.getElementById("live");
const picker = document.getElementById("state");

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function wordFor(rowId, status) {
  if (rowId === "approval") {
    if (status === "ok") return "given";
    if (status === "blocked") return "voided";
    return "not given";
  }
  return WORDS[status] ?? "unknown";
}

function renderRows() {
  rowsEl.replaceChildren(
    ...ROWS.map((row) => {
      const details = el("details", "rc-row");
      details.dataset.id = row.id;
      const summary = el("summary");
      const dot = el("span", "rc-dot");
      const text = el("span", "rc-row-text", row.text);
      const word = el("span", "rc-row-status");
      word.dataset.word = row.id;
      const hint = el("span", "rc-row-hint");
      hint.dataset.hint = row.id;
      summary.append(dot, text, word, hint);
      const body = el("div", "rc-row-body", BODY[row.id]);
      details.append(summary, body);
      return details;
    }),
  );
}

function clear(node) {
  while (node.firstChild) node.removeChild(node.firstChild);
}

function render(key) {
  const state = STATES[key];
  subEl.textContent = state.sub;
  summaryEl.textContent = state.summary ?? "";

  for (const row of ROWS) {
    const details = rowsEl.querySelector(`[data-id="${row.id}"]`);
    const status = state.rows[row.id] ?? "waiting";
    details.dataset.state = status;
    details.querySelector(`[data-hint="${row.id}"]`).textContent = state.hint[row.id] ?? "";
    details.querySelector(`[data-word="${row.id}"]`).textContent = wordFor(row.id, status);
  }

  clear(bannerSlot);
  if (state.banner) {
    const [tone, text] = state.banner;
    const banner = el("div", "rc-banner", text);
    banner.dataset.tone = tone;
    if (state.link) {
      const a = el("a", null, state.link[0]);
      a.href = state.link[1];
      a.rel = "noreferrer";
      banner.append(" ", a);
    }
    bannerSlot.append(banner);
  }
  if (state.progress) {
    const list = el("ol", "rc-progress");
    state.progress.forEach((step, index) => {
      const item = el("li", null, step);
      item.dataset.step = index < state.progressAt ? "done" : "todo";
      list.append(item);
    });
    bannerSlot.append(list);
  }

  clear(actionSlot);
  const make = (label, className, onClick) => {
    const button = el("button", className, label);
    button.type = "button";
    if (onClick) button.addEventListener("click", onClick);
    return button;
  };

  if (state.action === "stepup") {
    const panel = el("div", "rc-stepup");
    panel.append(el("p", null, "Confirm it's you. The board will not take an approval from an older sign-in."));
    panel.append(make("Confirm with passkey", "btn btn-primary btn-sm mt-2", () => render("merging")));
    bannerSlot.append(panel);
  }

  if (state.action === "approve") {
    actionSlot.append(make("Approve and merge", "btn btn-primary", () => render("step-up")));
  }
  if (state.action === "revoke") {
    actionSlot.append(make("Take my approval back", "btn btn-ghost btn-sm", () => render("ready")));
  }
  if (state.action === "retry") {
    actionSlot.append(make("Check again", "btn btn-ghost btn-sm", () => render("ready")));
  }
  if (state.action === "send-back") {
    actionSlot.append(make("Send back with a reason", "btn btn-ghost btn-sm"));
  }

  live.textContent = `${state.sub}. ${state.banner ? state.banner[1] : ""}`;
  picker.value = key;
  try {
    const params = new URLSearchParams(location.search);
    params.set("state", key);
    history.replaceState(null, "", `${location.pathname}?${params}`);
  } catch (e) { /* file:// is fine */ }
}

document.querySelectorAll("[data-theme-pick]").forEach((button) => {
  button.addEventListener("click", () => {
    const pick = button.dataset.themePick;
    if (pick) document.documentElement.setAttribute("data-theme", pick);
    else document.documentElement.removeAttribute("data-theme");
    try { localStorage.setItem("cadence-theme", pick || ""); } catch (e) { /* ignore */ }
    syncTheme();
  });
});

function syncTheme() {
  const current = document.documentElement.getAttribute("data-theme") || "";
  document.querySelectorAll("[data-theme-pick]").forEach((b) => {
    b.setAttribute("aria-pressed", String(b.dataset.themePick === current));
  });
}


picker.addEventListener("change", () => render(picker.value));
sheet.querySelector("button.x").addEventListener("click", () => live.focus?.());

renderRows();
syncTheme();
const initial = new URLSearchParams(location.search).get("state");
render(STATES[initial] ? initial : "ready");
