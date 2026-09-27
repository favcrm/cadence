/** Exercise the shared Wiki against the daemon's real JSON shapes. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/projects/cadence/context" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"]) {
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
}
for (const name of ["addEventListener", "removeEventListener"]) Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const moduleLoader = require("module");
const originalRequire = moduleLoader.prototype.require;
moduleLoader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const Wiki = (require("../src/features/wiki/Wiki") as typeof import("../src/features/wiki/Wiki")).default;
const Context = (require("../src/features/projects/Context") as typeof import("../src/features/projects/Context")).default;
const { navigate } = require("../src/lib/useLocation") as typeof import("../src/lib/useLocation");
const { wiki, WikiError } = require("../src/features/wiki/api") as typeof import("../src/features/wiki/api");
const { draftKey, stashDraft } = require("../src/features/wiki/editor") as typeof import("../src/features/wiki/editor");
const requests: string[] = [];
const writes: { path?: string; from?: string; to?: string; text?: string; if_rev?: string }[] = [];
let conflict = false;
let holdSave = false;
let finishSave: (() => void) | null = null;
let unavailable = false;
let missing = false;
const pagePath = "projects/cadence/README.md";
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
globalThis.fetch = async (input, init) => {
  const url = new URL(String(input), "http://localhost");
  requests.push(url.pathname + url.search);
  const path = url.searchParams.get("path") ?? "";
  if (init?.method === "PUT" || init?.method === "POST") {
    const body = JSON.parse(String(init.body));
    writes.push(body);
    if (conflict) return json({ conflict: "if_rev", current_rev: "newer-rev", path: body.path });
    if (holdSave) await new Promise<void>((resolve) => { finishSave = resolve; });
    return json({ path: body.path, rev: "saved-rev", committed: true });
  }
  if (unavailable) return json({ error: "This board needs a session" }, 403);
  if (url.pathname.endsWith("/ls")) {
    if (path.endsWith(".md") || path.endsWith(".png")) return json({ error: "a file, not a directory" }, 400);
    if (missing) return json({ error: `wiki ls '${path}': no such directory` }, 400);
    return json({ path, entries: [{ path: `${path}/README.md`, name: "README.md", kind: "text" }, { path: `${path}/research`, name: "research", kind: "dir" }, { path: `${path}/logo.png`, name: "logo.png", kind: "blob", mime: "image/png" }] });
  }
  if (url.pathname.endsWith("/file")) return json({ path, kind: "text", text: `# Project brief\n\nNotes for ${path}`, rev: "base-rev" });
  if (url.pathname.endsWith("/search")) return json({ matches: [
    { path: pagePath, line: 3, text: "Project brief match" },
    { path: "projects/other/private.md", line: 1, text: "outside project" },
    { path: pagePath, line: 7, text: "Second brief match" },
    { path: pagePath, line: 12, text: "Third brief match" },
    { path: pagePath, line: 15, text: "Fourth brief match" },
  ] });
  if (url.pathname.endsWith("/history")) return json({ path, commits: [{ sha: "commit1", at: 1, actor: "master", subject: "Saved project brief" }] });
  throw new Error(`Unexpected request: ${url}`);
};
function assert(value: unknown, what: string): asserts value { if (!value) throw new Error(what); }
const host = document.createElement("div");
document.body.append(host);
const view = createRoot(host);
const props = { project: "cadence", readOnly: false, actor: "master", onToast: () => {}, navHref: () => "/" };
const flush = async () => { await new Promise((resolve) => setTimeout(resolve, 10)); };
async function render(extra: Partial<typeof props> = {}) { await React.act(async () => { view.render(React.createElement(Context, { ...props, ...extra })); await flush(); }); await React.act(flush); }
async function go(href: string) { await React.act(async () => { navigate(href); await flush(); }); await React.act(flush); }
async function run() {
  await render();
  assert(location.search.includes("file=README.md"), "Context opens the project README first");
  assert(host.textContent?.includes("Notes for projects/cadence/README.md"), "raw daemon text renders as Markdown");
  assert(requests.every((url) => !url.endsWith("path=") && !url.includes("path=projects&")), "tree never requests global parents");
  assert(Array.from(host.querySelectorAll("a")).some((link) => link.textContent === "Edit" && link.href.includes("/context?") && link.href.includes("mode=edit")), "Edit stays in the Context tab");
  assert(!host.textContent?.includes("Repository references") && !host.querySelector(".context-references"), "Context contains only the shared Wiki workspace");
  assert(host.querySelectorAll(".wk-bar").length === 1 && host.querySelectorAll("h1").length === 1, "one toolbar and the document heading avoid duplicate page framing");
  const files = Array.from(host.querySelectorAll("button")).find((button) => button.textContent?.trim() === "Project files");
  assert(files?.getAttribute("aria-expanded") === "false", "shared explorer begins collapsed for mobile");
  await React.act(() => files.click());
  assert(files.getAttribute("aria-expanded") === "true", "files disclosure expands");
  const selected = host.querySelector(".wk-tname[aria-current='page']");
  assert(selected instanceof HTMLElement, "selected file is announced as current");
  await React.act(() => selected.click());
  assert(files.getAttribute("aria-expanded") === "false", "choosing a file closes the mobile explorer");
  stashDraft(sessionStorage, { path: pagePath, text: "Draft in this project", baseRev: "base-rev", at: 1 });
  await go("/projects/cadence/context?file=README.md&mode=edit");
  const source = host.querySelector("textarea");
  assert(source?.value === "Draft in this project", "existing Wiki draft recovery works in Context");
  const editorViews = host.querySelector("[aria-label='Editor view']");
  const writeView = editorViews?.querySelector("button:first-child");
  const previewView = editorViews?.querySelector("button:last-child");
  assert(writeView instanceof HTMLElement && previewView instanceof HTMLElement, "mobile editor exposes native view controls");
  await React.act(() => previewView.click());
  assert(previewView.getAttribute("aria-pressed") === "true" && writeView.getAttribute("aria-pressed") === "false", "view switch announces the selected pane");
  assert(host.querySelector("textarea") === source && source.value === "Draft in this project", "preview retains the same textarea and draft");
  assert(host.querySelector(".wk-eprev")?.textContent === "Draft in this project", "preview renders the current unsaved text");
  await React.act(() => writeView.click());
  assert(host.querySelector("textarea") === source && source.value === "Draft in this project", "switching back to Write preserves the draft");
  assert(Array.from(host.querySelectorAll("a")).some((link) => link.textContent === "Back to page") && !host.textContent?.includes("Cancel"), "return action does not imply the retained draft is discarded");
  conflict = true;
  const save = Array.from(host.querySelectorAll("button")).find((button) => button.textContent === "Save");
  assert(save, "Save control exists");
  await React.act(async () => { save.click(); await flush(); });
  assert(host.textContent?.includes("This page changed since you opened it. Your draft is kept.") && !host.textContent.includes("newer-rev"), "HTTP 200 conflict shows clear copy without an internal revision id");
  assert(source.value === "Draft in this project" && sessionStorage.getItem(draftKey(pagePath)), "a refused save preserves the draft");
  assert(writes[0]?.if_rev === "base-rev", "Save includes the base revision");
  const compare = Array.from(host.querySelectorAll("button")).find((button) => button.textContent === "Compare changes");
  assert(compare, "conflict offers a clear comparison action");
  await React.act(() => compare.click());
  assert(host.querySelector(".wk-diff")?.textContent?.includes("Draft in this project"), "comparison includes the retained draft");
  const discard = Array.from(host.querySelectorAll("button")).find((button) => button.textContent === "Discard draft and reload");
  assert(discard, "the draft-discarding action names its consequence");
  await React.act(async () => { discard.click(); await flush(); });
  assert(source.value.includes("Notes for projects/cadence/README.md") && !sessionStorage.getItem(draftKey(pagePath)) && save.disabled, "explicit discard loads the server copy, clears the draft and disables Save");
  await React.act(() => {
    Object.getOwnPropertyDescriptor(win.HTMLTextAreaElement.prototype, "value")?.set?.call(source, "Draft in this project");
    source.dispatchEvent(new win.Event("input", { bubbles: true }));
  });
  await React.act(() => previewView.click());
  conflict = false;
  holdSave = true;
  await React.act(async () => {
    previewView.dispatchEvent(new win.KeyboardEvent("keydown", { key: "s", ctrlKey: true, bubbles: true }));
    source.dispatchEvent(new win.KeyboardEvent("keydown", { key: "s", ctrlKey: true, bubbles: true }));
    await flush();
  });
  assert(writes.length === 2, "keyboard save from Preview works and repeated shortcuts send one pending write");
  assert(source.readOnly && save.getAttribute("aria-busy") === "true", "pending save shows progress and protects the submitted text");
  await React.act(async () => { finishSave?.(); await flush(); });
  holdSave = false;
  assert(!sessionStorage.getItem(draftKey(pagePath)) && !location.search.includes("mode=edit"), "a successful save clears the draft and returns to the project page");
  await go("/projects/cadence/context?file=README.md&mode=edit");
  await render({ readOnly: true });
  assert(host.querySelector("textarea")?.readOnly && Array.from(host.querySelectorAll("button")).find((button) => button.textContent === "Save")?.disabled, "read-only mode blocks editor and Save");
  await render({ readOnly: false });
  const typing = host.querySelector("textarea");
  assert(typing, "editor still mounted");
  await React.act(() => {
    Object.getOwnPropertyDescriptor(win.HTMLTextAreaElement.prototype, "value")?.set?.call(typing, "A last second edit");
    typing.dispatchEvent(new win.Event("input", { bubbles: true }));
  });
  assert(host.textContent?.includes("Unsaved changes"), "typing marks the draft dirty");
  await go("/projects/cadence/context?mode=search&q=brief");
  assert(sessionStorage.getItem(draftKey(pagePath))?.includes("A last second edit"), "leaving before the debounce preserves the last edit");
  assert(requests.some((url) => url.includes("/search?") && url.includes("path=projects%2Fcadence")), "search requests only this project");
  assert(host.textContent?.includes("Project brief match") && !host.textContent.includes("outside project"), "search results stay inside project");
  assert(host.querySelectorAll(".wk-srow").length === 1 && host.textContent.includes("4 matches"), "search renders one page with a count of its matches");
  assert(host.querySelectorAll(".wk-snip").length === 3 && !host.textContent.includes("Fourth brief match") && host.textContent.includes("1 more match in this file"), "search keeps excerpts compact without hiding the existence of further matches");
  assert(host.querySelector(".wk-sline")?.textContent === "Line 3" && host.querySelector("mark")?.textContent === "brief", "grouped excerpts preserve line numbers and query highlighting");
  const searchForm = host.querySelector(".wk-sinput");
  const searchInput = searchForm?.querySelector("input");
  assert(searchInput?.type === "search" && searchForm?.querySelector("button[type='submit']")?.textContent === "Search", "search has a visible native submit action");
  await React.act(async () => {
    Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")?.set?.call(searchInput, "research");
    searchInput.dispatchEvent(new win.Event("input", { bubbles: true }));
  });
  await React.act(async () => { searchForm.dispatchEvent(new win.Event("submit", { bubbles: true, cancelable: true })); await flush(); });
  assert(location.search.includes("q=research") && requests.some((url) => url.includes("q=research") && url.includes("path=projects%2Fcadence")), "submitting search updates the URL and keeps project scope");
  await go("/projects/cadence/context?file=README.md&mode=history");
  assert(host.textContent?.includes("Saved project brief"), "real commit log renders");
  assert(!host.textContent?.includes("Restore this version"), "unsupported restore is not offered");
  await go("/projects/cadence/context?file=logo.png");
  await render({ project: "assets" });
  assert(host.querySelector("img")?.getAttribute("src")?.includes("projects%2Fassets%2Flogo.png"), "pasted blob links preview using metadata, not a JSON read of bytes");
  await render({ project: "other" });
  assert(!host.textContent?.includes("Draft in this project"), "changing project clears prior editor state");
  const beforeInvalid = requests.length;
  await go("/projects/other/context?file=..%2Fprivate.md");
  assert(host.textContent?.includes("outside the project") && requests.length === beforeInvalid, "invalid path is refused before any request");
  unavailable = true;
  await go("/projects/other/context");
  assert(host.textContent?.includes("Sign in to view these pages"), "unsigned access has a clear sign-in state");
  unavailable = false;
  missing = true;
  await render({ project: "empty", readOnly: true });
  assert(host.textContent?.includes("No project context yet"), "missing project root has an empty state");
  assert(Array.from(host.querySelectorAll("button")).find((button) => button.textContent === "Create project context")?.disabled, "empty read-only context cannot create folders");
  assert(writes.length === 2, "mounting and reading never writes to initialize a root");
  missing = false;
  await wiki.mv(pagePath, "projects/cadence/brief.md");
  assert(writes[2]?.from === pagePath && writes[2]?.to === "projects/cadence/brief.md", "move sends the actual HTTP contract");
  conflict = true;
  try { await wiki.save(pagePath, "changed", "base"); throw new Error("conflict wrongly resolved"); } catch (error) { assert(error instanceof WikiError && error.status === 409, "successful conflict envelopes reject the save promise"); }
  conflict = false;
  await go("/projects/cadence/context?file=README.md&mode=edit");
  stashDraft(sessionStorage, { path: pagePath, text: "Pending navigation draft", baseRev: "base-rev", at: 2 });
  await render();
  const pendingSave = Array.from(host.querySelectorAll("button")).find((button) => button.textContent === "Save");
  assert(pendingSave && !pendingSave.disabled, "pending-navigation draft can be saved");
  holdSave = true;
  await React.act(async () => { pendingSave.click(); await flush(); });
  await go("/projects/cadence/context?mode=search&q=kept");
  stashDraft(sessionStorage, { path: pagePath, text: "Newer draft in another editor", baseRev: "base-rev", at: 3 });
  await React.act(async () => { finishSave?.(); await flush(); });
  holdSave = false;
  assert(location.search.includes("mode=search") && location.search.includes("q=kept"), "a completed save does not pull the operator away from a newer destination");
  assert(sessionStorage.getItem(draftKey(pagePath))?.includes("Newer draft in another editor"), "a late save does not erase a newer draft for the same page");
  await React.act(async () => {
    view.render(React.createElement(Wiki, { route: { screen: "wiki", mode: "browse", path: pagePath, query: null }, navHref: (route) => route.screen === "wiki" ? `/wiki/${route.mode}/${route.path ?? ""}` : "/", readOnly: true, actor: "master", onToast: () => {} }));
    await flush();
  });
  await React.act(flush);
  assert(host.querySelectorAll(".wk-bar").length === 1 && host.querySelectorAll("h1").length === 1, "global Wiki uses the same single-toolbar reader");
  assert(Array.from(host.querySelectorAll("button")).some((button) => button.textContent?.trim() === "Files"), "global Wiki uses the same collapsible explorer");
  assert(Array.from(host.querySelectorAll("button")).find((button) => button.textContent === "Edit")?.disabled, "shared reader respects read-only access in Wiki");
  await React.act(() => view.unmount());
  console.log("project Wiki interaction checks passed");
}
run().catch((error) => { console.error(error); throw error; });
export {};
