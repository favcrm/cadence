export {};
/** Lesson selection/lifetime and honest read states; Markdown is checked in Chrome. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/settings/memory" });
for (const name of [
  "window",
  "document",
  "Node",
  "Element",
  "HTMLElement",
  "HTMLInputElement",
  "SVGElement",
  "navigator",
  "MutationObserver",
  "Event",
  "MouseEvent",
  "KeyboardEvent",
  "location",
  "history",
]) {
  Object.defineProperty(globalThis, name, {
    value: name === "window" ? win : win[name],
    configurable: true,
    writable: true,
  });
}
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, {
    value: win[name].bind(win),
    configurable: true,
  });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module");
const originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id.endsWith("/Md"))
    return {
      __esModule: true,
      default: ({ text }: { text: string }) =>
        require("react").createElement("p", null, text),
    };
  if (id === "@hugeicons/core-free-icons")
    return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } =
  require("react-dom/client") as typeof import("react-dom/client");
const Memory = (
  require("../src/features/settings/Memory") as typeof import("../src/features/settings/Memory")
).default;
type Card = import("../src/lib/types").MemoryCard;
const card = (slug: string, project = "website"): Card => ({
  slug,
  project,
  fact: `Lesson ${slug}`,
  type: "rule",
  status: "accepted",
  confidence: "high",
  scope: {
    project: false,
    components: ["ui"],
    paths: ["src/**"],
    providers: [],
    tags: [],
  },
  created: "2026-09-25T00:00:00Z",
  path: `${project}/memory/${slug}.md`,
  quorum: { eligible: true, reason: "PM finalized this revision" },
  evidence: {
    state: "unverified",
    label: "unverified (last verified 2020-01-01)",
  },
});
const a = card("alpha"),
  b = card("beta"),
  withheld = {
    ...card("stale"),
    evidence: {
      state: "withheld",
      label: "withheld",
      reason: "citation changed",
    },
  };
const proposed = {
  ...card("proposal", "marketing"),
  status: "proposed",
  quorum: {
    eligible: false,
    accept: { eligible: true, reason: "Two receipts passed" },
  },
};
const unknown = { ...card("unknown"), quorum: null };
let cards = [a, b, withheld, proposed, unknown];
let failList = false,
  holdList = false;
let resolveList: ((r: Response) => void) | undefined;
let fileErrors: string[] = [];
let failDetail = false;
const requests: { slug: string; resolve: (r: Response) => void }[] = [];
const urls: string[] = [];
const json = (data: unknown, status = 200) =>
  new Response(JSON.stringify(data), {
    status,
    headers: { "Content-Type": "application/json" },
  });
globalThis.fetch = async (input, init) => {
  if (init?.method && init.method !== "GET")
    throw new Error("Memory must stay read-only");
  const url = new URL(String(input), "http://localhost");
  urls.push(url.pathname + url.search);
  if (url.pathname === "/api/memories") {
    if (holdList)
      return new Promise<Response>((resolve) => {
        resolveList = resolve;
      });
    if (failList) return json({ error: "List temporarily unavailable" }, 503);
    const selected = cards.filter((m) =>
      ["project", "status", "type"].every(
        (key) =>
          !url.searchParams.get(key) ||
          m[key as "project" | "status" | "type"] === url.searchParams.get(key),
      ),
    );
    return json({ memories: selected, memory_errors: fileErrors });
  }
  if (url.pathname.startsWith("/api/memories/")) {
    if (failDetail)
      return json({ error: "Detail temporarily unavailable" }, 503);
    return new Promise<Response>((resolve) =>
      requests.push({ slug: url.pathname.split("/").pop()!, resolve }),
    );
  }
  throw new Error(`Unexpected request ${input}`);
};
const host = document.createElement("div");
document.body.append(host);
let root = createRoot(host);
const assert: (v: unknown, why: string) => asserts v = (v, why) => {
  if (!v) throw new Error(why);
};
const flush = () =>
  React.act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
const onError = () => {};
const render = async (project = "all") => {
  await React.act(async () =>
    root.render(
      React.createElement(Memory, {
        project,
        projects: [{ key: "website" }, { key: "marketing" }],
        projectHref: (p: string) => `/settings/memory?project=${p}`,
        onError,
      }),
    ),
  );
  await flush();
};
const click = async (el?: Element | null) => {
  assert(el, "Click target exists");
  await React.act(async () =>
    el.dispatchEvent(new MouseEvent("click", { bubbles: true })),
  );
  await flush();
};
const button = (text: string) =>
  Array.from(document.querySelectorAll("button")).find(
    (el) => el.textContent?.trim() === text,
  );
const row = (text: string) =>
  Array.from(host.querySelectorAll(".memory-row-trigger")).find((el) =>
    el.textContent?.includes(`Lesson ${text}`),
  );
const choose = async (label: string, option: string) => {
  const control = Array.from(host.querySelectorAll("label")).find(
    (el) => el.textContent === label,
  );
  assert(control, "Filter label exists");
  await click(document.getElementById(control.htmlFor));
  await click(
    Array.from(document.querySelectorAll('[role="option"]')).find(
      (el) => el.textContent?.trim() === option,
    ),
  );
};
const fill = async (value: string) => {
  const input = host.querySelector('input[type="search"]');
  await React.act(async () => {
    Object.getOwnPropertyDescriptor(
      win.HTMLInputElement.prototype,
      "value",
    )!.set!.call(input, value);
    input!.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await flush();
};
const settle = async (index: number, memory: Card, body: string) => {
  await React.act(async () =>
    requests[index].resolve(json({ ...memory, body })),
  );
  await flush();
};
async function main() {
  holdList = true;
  await render();
  assert(
    host.textContent?.includes("Loading project memory") &&
      !host.textContent?.includes("No lessons yet"),
    "Pending list is distinct from empty",
  );
  holdList = false;
  await React.act(async () => resolveList!(json({ memories: cards })));
  await flush();
  assert(
    host.querySelectorAll("article").length === 5,
    "All lesson rows render",
  );
  assert(
    row("stale")?.textContent?.includes("Withheld from agents"),
    "Accepted stale evidence stays withheld",
  );
  assert(
    row("proposal")?.textContent?.includes("Awaiting PM finalization"),
    "Passing proposal receipts do not mean available",
  );
  assert(
    row("unknown")?.textContent?.includes("Verification unavailable"),
    "Missing quorum is unknown",
  );
  await fill("SRC/**");
  assert(
    host.querySelectorAll("article").length === 5,
    "Scope search is case insensitive",
  );
  await fill("alpha");
  assert(
    host.querySelectorAll("article").length === 1,
    "Claim and ID search narrows rows",
  );
  await click(button("Clear filters"));
  await choose("Status", "Proposed");
  assert(
    host.querySelectorAll("article").length === 1 &&
      urls.at(-1)?.includes("status=proposed"),
    "Status remains a server filter",
  );
  await click(button("Clear filters"));
  await choose("Type", "Gotcha");
  assert(
    host.textContent?.includes("No matching lessons"),
    "Empty filter result is clear",
  );
  await click(button("Show all lessons"));

  await click(row("alpha"));
  assert(
    row("alpha")?.getAttribute("aria-expanded") === "true",
    "Row disclosure announces expanded",
  );
  await click(row("beta"));
  await click(row("alpha"));
  await settle(2, a, "Newest alpha body");
  await settle(0, a, "Obsolete alpha body");
  await settle(1, b, "Late beta body");
  assert(
    host.textContent?.includes("Newest alpha body") &&
      !host.textContent?.includes("Obsolete") &&
      !host.textContent?.includes("Late beta"),
    "A→B→A drops previous selection requests, even the same key",
  );
  assert(
    host.textContent?.includes("unverified (last verified 2020-01-01)") &&
      row("alpha")?.textContent?.includes("Available to agents"),
    "Aged evidence remains available with server freshness label",
  );
  const details = host.querySelector(
    ".memory-provenance",
  ) as HTMLDetailsElement;
  assert(
    details && !details.open && details.textContent?.includes("alpha"),
    "Technical evidence is retained in a collapsed native disclosure",
  );
  await click(row("alpha"));
  await click(row("alpha"));
  await click(row("alpha"));
  await click(row("alpha"));
  await settle(4, a, "Reopened current body");
  await settle(3, a, "Closed old body");
  assert(
    !host.textContent?.includes("Closed old body"),
    "Close/reopen invalidates same-key read",
  );
  await click(row("beta"));
  await fill("alpha");
  await settle(5, b, "Hidden beta");
  await fill("");
  assert(
    !host.textContent?.includes("Hidden beta"),
    "Hiding a lesson by search invalidates its response",
  );
  await settle(6, b, "Visible beta");
  await click(button("Refresh lessons"));
  assert(
    !host.textContent?.includes("Visible beta"),
    "Refresh closes current detail",
  );

  failDetail = true;
  await click(row("alpha"));
  assert(button("Retry lesson"), "Detail failure offers retry");
  failDetail = false;
  await click(button("Retry lesson"));
  await settle(7, a, "Recovered body");
  assert(host.textContent?.includes("Recovered body"), "Retry recovers detail");
  await click(row("beta"));
  await render("marketing");
  await settle(8, b, "Wrong project detail");
  assert(
    host.querySelectorAll("article").length === 1 &&
      !host.textContent?.includes("Wrong project"),
    "Route project isolates list and invalidates detail",
  );
  await choose("Project", "website");
  assert(
    location.search === "?project=website",
    "Project choice uses route navigation",
  );
  await render("all");
  fileErrors = ["broken.md: invalid scope", "other.md: cannot read"];
  await click(button("Refresh lessons"));
  assert(
    host.querySelectorAll("article").length === 5 &&
      host.textContent?.includes("Results may be incomplete") &&
      host.querySelector(".memory-load-warning details"),
    "Partial files retain good lessons and disclosed errors",
  );
  await fill("absent claim");
  assert(
    host.textContent?.includes("No readable matches") &&
      button("Retry lessons"),
    "Incomplete search results never claim there are no matches in failed files",
  );
  await click(button("Clear filters"));
  failList = true;
  await click(button("Refresh lessons"));
  assert(
    host.textContent?.includes("Couldn’t load lessons") &&
      button("Retry lessons"),
    "Failed list is recoverable rather than empty",
  );
  failList = false;
  await click(button("Retry lessons"));
  assert(
    host.querySelectorAll("article").length === 5,
    "List retry recovers rows",
  );
  cards = [];
  await click(button("Refresh lessons"));
  assert(
    host.textContent?.includes("No readable lessons") &&
      host.textContent?.includes("0 readable lessons · results incomplete") &&
      !host.textContent?.includes("No lessons yet") &&
      button("Retry lessons"),
    "All failed files never masquerade as a valid empty store",
  );
  cards = [a];
  fileErrors = [];
  await click(button("Retry lessons"));
  assert(
    host.querySelectorAll("article").length === 1 &&
      !host.textContent?.includes("incomplete"),
    "Retry recovers all-file failure state",
  );
  cards = [];
  fileErrors = [];
  await click(button("Refresh lessons"));
  assert(
    host.textContent?.includes("No lessons yet") &&
      !host.textContent?.includes("No matching"),
    "Resolved empty store explains native curation",
  );
  cards = [a];
  await click(button("Refresh lessons"));
  await click(row("alpha"));
  await React.act(async () => root.unmount());
  await settle(9, a, "After unmount");
  assert(host.textContent === "", "Unmount drops pending detail");
  console.log("memory workspace checks passed");
}
main().catch((error) => {
  throw error;
});
