import { createScreenReceiver } from "@cadence/app-kit";

const root = document.querySelector("#root") ?? document.body;
const state = { connected: false, rights: new Set(), records: [], next: null, loaded: false, busy: false, draft: "", error: null };
const element = (tag, text, className) => {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = text;
  if (className) node.className = className;
  return node;
};

function render() {
  const main = element("main");
  main.append(element("h1", "Private checklist"));
  main.append(element("p", state.connected ? "Tasks are stored in this workspace." : "Open this app in a workspace to connect."));
  const form = element("div", undefined, "composer");
  const input = element("input");
  input.type = "text"; input.maxLength = 200; input.placeholder = "Add a task";
  input.setAttribute("aria-label", "Task title"); input.value = state.draft;
  input.disabled = state.busy || !state.rights.has("records.create");
  input.addEventListener("input", () => { state.draft = input.value; });
  const add = element("button", "Add task");
  add.type = "button"; add.disabled = input.disabled;
  add.addEventListener("click", () => void createTask(input.value));
  input.addEventListener("keydown", (event) => { if (event.key === "Enter") { event.preventDefault(); void createTask(input.value); } });
  form.append(input, add); main.append(form);
  if (state.error) { const error = element("p", state.error, "error"); error.setAttribute("role", "alert"); main.append(error); }
  if (state.connected && !state.rights.has("records.list")) main.append(element("p", "You cannot read tasks in this workspace."));
  else if (state.busy && !state.loaded) main.append(element("p", "Loading tasks…"));
  else if (state.loaded && state.records.length === 0) main.append(element("p", "No tasks yet."));
  const list = element("ul");
  for (const record of state.records) {
    const row = element("li");
    row.append(element("span", record.title, record.status === "done" ? "done" : undefined));
    const toggle = element("button", record.status === "done" ? "Reopen" : "Complete");
    toggle.type = "button"; toggle.disabled = state.busy || !state.rights.has("records.update");
    toggle.setAttribute("aria-label", `${toggle.textContent}: ${record.title}`);
    toggle.addEventListener("click", () => void updateTask(record));
    row.append(toggle); list.append(row);
  }
  main.append(list);
  if (state.next) {
    const more = element("button", "Load more");
    more.type = "button"; more.disabled = state.busy;
    more.addEventListener("click", () => void load(true)); main.append(more);
  }
  const refresh = element("button", "Refresh");
  refresh.type = "button"; refresh.disabled = state.busy || !state.rights.has("records.list");
  refresh.addEventListener("click", () => void load(false)); main.append(refresh);
  root.replaceChildren(main);
}

async function load(append) {
  if (state.busy || !state.rights.has("records.list")) return;
  state.busy = true; state.error = null; render();
  try {
    const page = await receiver.call("records.list", { limit: 50, ...(append && state.next ? { cursor: state.next } : {}) });
    if (!state.rights.has("records.list")) return;
    if (!page || !Array.isArray(page.records) || page.records.length > 50 ||
        !(page.next_cursor === null || typeof page.next_cursor === "string")) throw new Error("invalid page");
    const records = page.records.map((item) => {
      if (!item || typeof item.id !== "string" || !Number.isSafeInteger(item.revision) || item.revision < 1 ||
          !item.profile || typeof item.profile.title !== "string" || !["open", "done"].includes(item.profile.status))
        throw new Error("invalid task");
      return { id: item.id, revision: item.revision, title: item.profile.title, status: item.profile.status };
    });
    state.records = append ? [...state.records, ...records] : records;
    state.next = page.next_cursor; state.loaded = true;
  } catch { state.error = "Tasks could not be read. Refresh to try again."; }
  finally { state.busy = false; render(); }
}

async function createTask(value) {
  const title = value.trim();
  if (!title || state.busy || !state.rights.has("records.create")) return;
  state.busy = true; state.error = null; render();
  try {
    await receiver.call("records.create", { record_id: `task-${crypto.randomUUID()}`, profile: { title, status: "open" } });
    state.draft = "";
  } catch { state.error = "The result could not be confirmed. Refresh before trying again."; }
  finally { state.busy = false; render(); }
  if (!state.error) await load(false);
}

async function updateTask(record) {
  if (state.busy || !state.rights.has("records.update")) return;
  state.busy = true; state.error = null; render();
  try {
    await receiver.call("records.update", { record_id: record.id, expected_revision: record.revision,
      profile: { status: record.status === "done" ? "open" : "done" } });
  } catch { state.error = "The result could not be confirmed. Refresh before trying again."; }
  finally { state.busy = false; render(); }
  if (!state.error) await load(false);
}

const receiver = createScreenReceiver({
  accepts: ["screen.v2", "screen-records.v1"],
  onPush(push) {
    const next = new Set(Array.isArray(push.actions) ? push.actions.filter((item) => typeof item === "string") : []);
    const hadRead = state.rights.has("records.list");
    const changed = !state.connected || next.size !== state.rights.size || [...next].some((right) => !state.rights.has(right));
    if (!changed) return;
    state.connected = true; state.rights = next;
    if (!next.has("records.list")) { state.records = []; state.next = null; state.loaded = false; }
    render();
    if (!hadRead && next.has("records.list")) queueMicrotask(() => void load(false));
  },
  onError() { state.connected = false; state.rights.clear(); state.records = []; state.loaded = false; state.error = "The workspace connection closed."; render(); },
});
render();
