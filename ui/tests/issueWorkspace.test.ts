import type * as EvidenceModule from "../src/features/issues/Evidence";
import type * as SessionKeyModule from "../src/lib/sessionKey";
import type * as PrivateSessionModule from "../src/lib/privateSession";
export {};
/** Real Setup and shared stores: loading, readiness, project refresh and copy feedback. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/setup" });
for (const name of [
  "window",
  "document",
  "Node",
  "Element",
  "HTMLElement",
  "SVGElement",
  "navigator",
  "MutationObserver",
  "Event",
  "MouseEvent",
  "location",
  "history",
  "FileReader",
  "sessionStorage",
])
  Object.defineProperty(globalThis, name, {
    value: name === "window" ? win : win[name],
    configurable: true,
    writable: true,
  });
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, {
    value: win[name].bind(win),
    configurable: true,
  });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module"),
  originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons")
    return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } =
  require("react-dom/client") as typeof import("react-dom/client");
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const IssuePageComponent = require("../src/features/issues/IssuePage")
  .default as typeof import("../src/features/issues/IssuePage").default;
const IssuePage = (props: React.ComponentProps<typeof IssuePageComponent>) =>
  React.createElement(LocaleProvider, null, React.createElement(IssuePageComponent, props));
const { resources } =
  require("../src/lib/resources") as typeof import("../src/lib/resources");
type Detail = import("../src/lib/types").IssueDetail;
const detail = (id: string): Detail => ({
  id,
  project: "cadence",
  title: `Work ${id}`,
  status: "done",
  status_source: "file",
  priority: "P2",
  blocked_by: [],
  relates: [],
  refs: [],
  container: false,
  ready: false,
  blocked: false,
  created: "2026-09-26T00:00:00Z",
  rev: "r1",
  paths: [], tags: [], work: { type: "task", type_source: "implicit", weight: 1 },
  checks: { done: 0, total: 1 },
  frontmatter: {},
  body: "Goal text.\n\n## Acceptance\n- [ ] Unique criterion",
  path: "issue.md",
  links: {
    children: [],
    blocked_by: [],
    blocks: [],
    relates: [],
    duplicates: [],
  },
  comments: [],
  notes_chain: [],
  artifacts: [],
  activity: [],
});
const details: Record<string, Detail> = {
  "CAD-1": detail("CAD-1"),
  "CAD-2": detail("CAD-2"),
};
const errors = new Set<string>();
const held = new Set<string>();
const pending: { path: string; resolve: (r: Response) => void }[] = [];
let comments = 0;
let enforceRevision = false;
const historyReads: Record<string, number> = {};
const json = (data: unknown, status = 200) =>
  new Response(JSON.stringify(data), {
    status,
    headers: { "Content-Type": "application/json" },
  });
globalThis.fetch = async (input, init) => {
  const path = new URL(String(input), "http://localhost").pathname;
  const id = path.split("/")[3];
  if (init?.method === "POST") {
    comments++;
    const request = JSON.parse(String(init.body));
    if (enforceRevision && request.if_rev !== details[id].rev)
      return json({ error: "Issue changed; re-read and retry" }, 409);
    if (held.has("comment"))
      return new Promise<Response>((resolve) =>
        pending.push({ path: "comment", resolve }),
      );
    return errors.has("comment")
      ? json({ error: "Comment temporarily unavailable" }, 503)
      : json({ issue: details[id] });
  }
  if (held.has(path))
    return new Promise<Response>((resolve) => pending.push({ path, resolve }));
  if (errors.has(path)) return json({ error: `Sample failure: ${path}` }, 503);
  if (path.endsWith("/history")) {
    historyReads[id] = (historyReads[id] ?? 0) + 1;
    return json({
      id,
      history: [
        {
          sha: "a",
          at: "2026-09-27T00:00:00Z",
          by: "operator",
          kind: "set",
          summary: `History for ${id} at ${details[id]?.rev}`,
        },
      ],
    });
  }
  if (path.endsWith("/lane"))
    return json({ issue: id, lane: null, providers: [] });
  if (details[id]) return json(details[id]);
  throw new Error(`Unexpected request ${path}`);
};
const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const assert: (v: unknown, why: string) => asserts v = (v, why) => {
  if (!v) throw new Error(why);
};
const flush = async () => {
  for (let i = 0; i < 3; i++)
    await React.act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
};
const text = () => host.textContent ?? "";
const button = (name: RegExp) =>
  Array.from(host.querySelectorAll("button")).find((b) =>
    name.test(b.textContent?.trim() ?? ""),
  );
const click = async (el: Element | undefined | null) => {
  assert(el, "control exists");
  await React.act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await flush();
};
let id = "CAD-1",
  tab: import("../src/features/issues/model").IssueTab = "overview";
const render = async () => {
  await React.act(async () =>
    root.render(
      React.createElement(IssuePage, {
        id,
        project: "cadence",
        tab,
        tabHref: (t) => `/issues/${id}?tab=${t}`,
        issues: [],
        agents: null,
        readOnly: false,
        writeBlock: null,
        kickoffBlock: "Operator required",
        onWrite: () => {},
        onError: () => {},
        onOpen: () => {},
        planHref: "/",
        onToast: () => {},
      }),
    ),
  );
  await flush();
};
const write = async (value: string) => {
  const input = host.querySelector("textarea");
  assert(input, "comment editor exists");
  await React.act(async () => {
    Object.getOwnPropertyDescriptor(
      win.HTMLTextAreaElement.prototype,
      "value",
    )!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await flush();
};
const scenario = require("process").argv[2] ?? "all";
async function run() {
  await render();
  if (scenario === "all" || scenario === "cold_history") {
    details["CAD-4"] = detail("CAD-4");
    id = "CAD-4";
    tab = "activity";
    held.add("/api/issues/CAD-4");
    await render();
    assert(
      historyReads[id] === 1,
      "History read settles independently while initial detail waits",
    );
    details[id] = { ...details[id], rev: "r2" };
    held.clear();
    await React.act(async () =>
      pending.splice(0).forEach((p) => p.resolve(json(details[id]))),
    );
    await flush();
    assert(
      text().includes("History for CAD-4 at r2"),
      "Initial detail revision refreshes independently loaded old history",
    );
    id = "CAD-1";
    tab = "overview";
    await render();
  }
  if (scenario === "all" || scenario === "conflict") {
    details["CAD-5"] = detail("CAD-5");
    id = "CAD-5";
    tab = "activity";
    await render();
    await write("Preserve this conflict draft");
    details[id] = { ...details[id], rev: "r2" };
    enforceRevision = true;
    const before = comments;
    await click(button(/^Post comment$/));
    assert(
      host.querySelector("textarea")?.value === "Preserve this conflict draft",
      "409 preserves the comment draft",
    );
    assert(
      resources.issue(id).get().data?.rev === "r2",
      "409 re-reads detail for a current revision before manual retry",
    );
    assert(
      comments === before + 1,
      "Conflict recovery never automatically repeats a write",
    );
    await click(button(/^Post comment$/));
    assert(
      comments === before + 2 && host.querySelector("textarea")?.value === "",
      "Manual retry sends the refreshed revision and succeeds",
    );
    enforceRevision = false;
    id = "CAD-1";
    tab = "overview";
    await render();
  }
  if (scenario === "all" || scenario === "conflict_inflight") {
    details["CAD-6"] = detail("CAD-6");
    id = "CAD-6";
    tab = "activity";
    await render();
    const oldDetail = { ...details[id] };
    held.add("/api/issues/CAD-6");
    await React.act(async () => {
      void resources.issue(id).refresh();
    });
    await flush();
    details[id] = { ...details[id], rev: "r2" };
    enforceRevision = true;
    await write("Conflict during an older read");
    await click(button(/^Post comment$/));
    held.clear();
    await React.act(async () =>
      pending.splice(0).forEach((p) => p.resolve(json(oldDetail))),
    );
    await flush();
    assert(
      resources.issue(id).get().data?.rev === "r2",
      "409 invalidates a read that started before the conflict and waits for its fresh follow-up",
    );
    await click(button(/^Post comment$/));
    assert(
      host.querySelector("textarea")?.value === "",
      "Conflict during a stale inflight read recovers on explicit retry",
    );
    enforceRevision = false;
    id = "CAD-1";
    tab = "overview";
    await render();
  }
  if (scenario === "all" || scenario === "initial") {
    details["CAD-3"] = detail("CAD-3");
    id = "CAD-3";
    held.add("/api/issues/CAD-3");
    await render();
    assert(
      text().includes("Loading issue"),
      "Initial issue read shows loading",
    );
    held.clear();
    await React.act(async () =>
      pending
        .splice(0)
        .forEach((p) =>
          p.resolve(json({ error: "Initial issue unavailable" }, 503)),
        ),
    );
    await flush();
    assert(
      text().includes("Issue could not be loaded.") &&
        !text().includes("Work CAD-3"),
      "Initial failure does not paint an issue",
    );
    await click(button(/Retry issue/));
    assert(text().includes("Work CAD-3"), "Initial failure retry recovers");
    id = "CAD-1";
    await render();
  }
  if (scenario === "all" || scenario === "issue") {
    errors.add("/api/issues/CAD-1");
    await React.act(async () => resources.issue(id).refresh());
    assert(
      text().includes("Issue could not be loaded.") &&
        text().includes("Showing the last known information."),
      "Issue refresh failure labels retained content",
    );
    errors.clear();
    await click(button(/Retry issue/));
    assert(
      !text().includes("Issue could not be loaded."),
      "Issue retry recovers",
    );
  }
  if (scenario === "all" || scenario === "content") {
    assert(
      Array.from(host.querySelectorAll("h2")).filter(
        (h) => h.textContent === "Acceptance",
      ).length === 1,
      "Acceptance renders exactly once",
    );
    assert(!button(/^Approve$/), "Unsupported approval action is absent");
    assert(
      !host.querySelector('[role="tab"]'),
      "Issue sections are native page links",
    );
  }
  if (scenario === "all" || scenario === "history") {
    tab = "activity";
    errors.add("/api/issues/CAD-1/history");
    await React.act(async () =>
      resources.issue(id).write(() => ({ ...details[id], rev: "r2" })),
    );
    await render();
    assert(
      /history.*could not|could not.*history/i.test(text()),
      "Failed history is visible",
    );
    assert(
      !/No activity yet/.test(text()),
      "Failed history cannot claim an empty timeline",
    );
    errors.clear();
    await click(button(/Retry history/));
    assert(text().includes("History for CAD-1"), "History retry recovers");
  }
  if (scenario === "all" || scenario === "lane") {
    errors.add("/api/issues/CAD-1/lane");
    await React.act(async () => resources.lane(id).refresh());
    assert(
      /lane.*could not|could not.*lane/i.test(text()),
      "Failed lane refresh is visible",
    );
    assert(
      !text().includes("No lane yet"),
      "Failed lane read never claims no lane",
    );
    errors.clear();
    await click(button(/Retry lane/));
  }
  if (scenario === "all" || scenario === "navigation") {
    tab = "activity";
    await render();
    assert(
      text().includes("History for CAD-1"),
      "First issue history available",
    );
    await write("First issue draft");
    await React.act(async () =>
      resources.issue("CAD-2").write(() => details["CAD-2"]),
    );
    held.add("/api/issues/CAD-2/history");
    errors.add("/api/issues/CAD-2/lane");
    id = "CAD-2";
    await render();
    assert(
      !text().includes("History for CAD-1"),
      "Old issue history is absent while new history loads",
    );
    assert(
      host.querySelector("textarea")?.value === "",
      "Comment draft resets across issues",
    );
    assert(
      text().includes("Lane could not be loaded.") &&
        !text().includes("No lane yet"),
      "Initial lane failure is distinct from absence",
    );
    errors.clear();
    held.clear();
    await React.act(async () =>
      pending.splice(0).forEach((p) => p.resolve(json({ id, history: [] }))),
    );
    await flush();
  }
  if (scenario === "all" || scenario === "comment") {
    tab = "activity";
    await render();
    await write("Keep my draft");
    assert(
      host.querySelector("textarea")?.getAttribute("aria-label") ===
        "Comment" ||
        host.querySelector("label[for]")?.textContent?.includes("Comment"),
      "Comment has an accessible label",
    );
    held.add("comment");
    const before = comments;
    await click(button(/^(Post comment|Comment)$/));
    await click(button(/^(Posting|Post comment|Comment)/));
    assert(comments === before + 1, "Pending comment cannot double-submit");
    held.clear();
    await React.act(async () =>
      pending
        .splice(0)
        .forEach((p) =>
          p.resolve(json({ error: "Comment temporarily unavailable" }, 503)),
        ),
    );
    await flush();
    assert(
      host.querySelector("textarea")?.value === "Keep my draft" &&
        text().includes("Comment temporarily unavailable"),
      "Failed comment preserves draft and visible error",
    );
    await click(button(/^Post comment$/));
    assert(
      host.querySelector("textarea")?.value === "",
      "Successful retry clears draft",
    );
  }
  if (scenario === "all" || scenario === "pr") {
    details[id] = {
      ...details[id],
      reports: [
        {
          name: "review.md",
          body: "Review evidence before PR",
          kind: "review",
        },
      ],
    };
    await React.act(async () => resources.issue(id).write(() => details[id]));
    tab = "pr";
    await render();
    assert(
      text().includes("Review evidence before PR"),
      "Reports remain visible before a PR exists",
    );
    assert(
      !text().includes("Checks, verdicts, and the merge queue show up"),
      "Empty PR does not promise unsupported data",
    );
  }
  if (scenario === "all" || scenario === "evidence") {
    const { Evidence } = require("../src/features/issues/Evidence") as typeof EvidenceModule;
    const { setSessionKey } = require("../src/lib/sessionKey") as typeof SessionKeyModule;
    const { allowPrivateReads } = require("../src/lib/privateSession") as typeof PrivateSessionModule;
    const originalFetch = globalThis.fetch;
    const originalCreate = URL.createObjectURL;
    const originalRevoke = URL.revokeObjectURL;
    const created: string[] = [];
    const revoked: string[] = [];
    const requests: { path: string; init?: RequestInit; resolve: (response: Response) => void }[] = [];
    URL.createObjectURL = () => {
      const url = `blob:http://localhost/artifact-${created.length}`;
      created.push(url);
      return url;
    };
    URL.revokeObjectURL = (url) => { revoked.push(url); };
    globalThis.fetch = (input, init) => new Promise<Response>((resolve) => {
      requests.push({ path: String(input), init, resolve });
    });
    allowPrivateReads();
    setSessionKey("artifact-session-secret");
    const artifactResponse = (mime: string) => {
      const response = new Response(null, { headers: { "Content-Type": mime } });
      // happy-dom's FileReader consumes its own Blob implementation.
      response.blob = async () => new win.Blob(["artifact bytes"], { type: mime }) as Blob;
      return response;
    };
    const renderEvidence = async (issue: string) => {
      await React.act(async () => root.render(React.createElement(Evidence, {
        id: issue,
        images: [{ name: "screen shot.png", size: 14 }],
        files: [{ name: "report.html", size: 14 }],
        readOnly: true,
        onWrite: () => {},
        onError: () => {},
      })));
      await flush();
    };
    try {
      await renderEvidence("CAD-1");
      assert(requests.length === 2, "Evidence fetches each artifact through the private transport");
      for (const request of requests) {
        assert(new Headers(request.init?.headers).get("X-Cadence-Session") === "artifact-session-secret", "Artifact reads carry the tab session header");
        assert(request.init?.credentials === "same-origin", "Artifact reads retain the existing cookie");
        assert(!request.path.includes("artifact-session-secret") && !request.path.includes("?"), "Artifact URLs never carry credentials");
      }
      assert(requests[0].path === "/api/issues/CAD-1/artifacts/screen%20shot.png", "Artifact filenames remain encoded in the private request");
      assert(host.querySelector('[role="status"]'), "Pending artifact reads have accessible loading status");
      assert(!host.querySelector("a[href], img[src]"), "No browser element navigates to the private file route");
      await React.act(async () => {
        requests[0].resolve(artifactResponse("image/png"));
        requests[1].resolve(artifactResponse("application/octet-stream"));
      });
      await flush();
      const image = host.querySelector("img");
      assert(image !== null, "Authenticated screenshot bytes mount a preview image");
      assert(image.src.startsWith("data:image/png;") && image.alt === "screen shot.png", "Screenshots retain CSP-compatible previews and their accessible filename");
      const imageLink = image.closest("a");
      assert(imageLink?.href.startsWith("blob:") && imageLink.target === "_blank" && !imageLink.hasAttribute("download"), "Only raster screenshot blobs retain inline view");
      const download = host.querySelector('a[download="report.html"]');
      assert(download?.getAttribute("href")?.startsWith("blob:") && download.textContent === "report.html" && !download.hasAttribute("target"), "Active attachments download authenticated bytes with the original filename");

      await renderEvidence("CAD-2");
      assert(created.every((url) => revoked.includes(url)), "Navigating between issues revokes every previous object URL");
      assert(!host.querySelector("img, a[href]"), "New issue never displays the previous issue's bytes");
      await React.act(async () => {
        requests[2].resolve(artifactResponse("image/svg+xml"));
        requests[3].resolve(new Response(null, { status: 503 }));
      });
      await flush();
      assert(!host.querySelector("img"), "Unexpected active image MIME never becomes a preview");
      assert(host.querySelector('a[download="screen shot.png"]'), "Unexpected active image MIME downloads instead of opening inline");
      assert(host.querySelector('[role="alert"]')?.textContent?.includes("report.html"), "Failed reads identify the attachment accessibly");
      const retry = host.querySelector('button[aria-label="Retry report.html"]');
      await click(retry);
      assert(Number(requests.length) === 5, "Retry fetches the failed attachment again");
      await React.act(async () => requests[4].resolve(artifactResponse("text/plain")));
      await flush();
      assert(host.querySelector('a[download="report.html"]'), "Retry restores a usable attachment download");

      await renderEvidence("CAD-3");
      const countBeforeUnmount = created.length;
      await React.act(async () => root.render(null));
      assert(requests[5].init?.signal?.aborted && requests[6].init?.signal?.aborted, "Unmount aborts pending artifact reads");
      await React.act(async () => {
        requests[5].resolve(artifactResponse("image/png"));
        requests[6].resolve(artifactResponse("text/plain"));
      });
      await flush();
      assert(created.length === countBeforeUnmount, "Late responses cannot create URLs after their effect is disposed");
      assert(created.every((url) => revoked.includes(url)), "Unmount leaves no owned artifact object URLs");
    } finally {
      await React.act(async () => root.render(null));
      globalThis.fetch = originalFetch;
      URL.createObjectURL = originalCreate;
      URL.revokeObjectURL = originalRevoke;
      setSessionKey(null);
    }
  }
  await React.act(async () => root.unmount());
  await win.happyDOM.close();
  console.log(`issueWorkspace ${scenario}: passed`);
}
run().catch(async (e) => {
  await React.act(async () => root.unmount());
  await win.happyDOM.close();
  console.error(e);
  throw e;
});
