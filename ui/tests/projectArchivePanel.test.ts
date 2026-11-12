/** CAD-2255: the archive panel must never report an unproved transition. */
import type * as HappyDom from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDomClient from "react-dom/client";
import type * as Locale from "../src/lib/locale";
import type * as PrivateSession from "../src/lib/privateSession";
import type * as ProjectSettingsModule from "../src/features/projects/ProjectSettings";
import type * as ConfirmDialogModule from "../src/features/app-shell/shared/ConfirmDialog";
import type * as ApiModule from "../src/lib/api";
import type * as ResourcesModule from "../src/lib/resources";

declare function require(name: string): unknown;

function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}
let act: typeof ReactModule.act;
async function settle(check: () => void) {
  for (let n = 0; n < 200; n++) {
    try {
      check();
      return;
    } catch (error) {
      if (n === 199) throw error;
      await act(async () => {
        await Promise.resolve();
      });
    }
  }
}

type Json = (value: unknown, status?: number) => Response;

/** The test runner uses Node 22; this project's ES2023 library omits the built-in type. */
const deferred = Promise as typeof Promise & {
  withResolvers<T>(): { promise: Promise<T>; resolve: (value: T) => void };
};

/** One streamed lifecycle receipt, built the way the server answers it. */
function receipt(project: string, over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    archive_capability: "cad2255.project-archive.v1",
    project,
    archived: false,
    revision: 1,
    archived_at: "2026-01-01T00:00:00Z",
    archived_by: "owner@example.test",
    changed_at: "2026-01-02T00:00:00Z",
    changed_by: "operator@example.test",
    blockers: [],
    ...over,
  };
}

async function main() {
  const { Window } = require("happy-dom") as typeof HappyDom;
  const win = new Window({ url: "http://localhost/projects/sample/settings" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage", "localStorage"] as const)
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener"] as const)
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  Object.defineProperty(globalThis, "requestAnimationFrame", { value: (fn: FrameRequestCallback) => { fn(0); return 0; }, configurable: true });
  const loader = require("module") as { prototype: { require: (this: unknown, id: string) => unknown } };
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string): unknown {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.call(this, id);
  };
  const React = require("react") as typeof ReactModule;
  act = React.act;
  const { createRoot } = require("react-dom/client") as typeof ReactDomClient;
  const { LocaleProvider } = require("../src/lib/locale") as typeof Locale;
  const session = require("../src/lib/privateSession") as typeof PrivateSession;
  const { resources } = require("../src/lib/resources") as typeof ResourcesModule;
  const ProjectSettings = (require("../src/features/projects/ProjectSettings") as typeof ProjectSettingsModule).default;
  const ConfirmDialog = (require("../src/features/app-shell/shared/ConfirmDialog") as typeof ConfirmDialogModule).default;
  session.allowPrivateReads();

  const json: Json = (value, status = 200) =>
    new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });

  /* --------------------------- the mocked board --------------------------- */
  const lifecycles = new Map<string, Record<string, unknown>>();
  const lifecycleStatus = new Map<string, number>();
  const policyMode = new Map<string, "recorded" | "not_stored">();
  const archiveSent: { project: string; verb: string; confirmation: unknown; expected_revision: unknown }[] = [];
  let archiveHold: { promise: Promise<Response>; resolve: (value: Response) => void } | null = null;
  let lifecycleHold: { promise: Promise<Response>; resolve: (value: Response) => void } | null = null;
  let archiveReply: ((project: string, verb: string) => Response) | null = null;

  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = new URL(typeof input === "string" ? input : String((input as Request).url), "http://localhost");
    const method = init?.method ?? "GET";
    if (url.pathname === "/api/team/members") return json({ members: [] });
    const matched = /^\/api\/projects\/([^/]+)\/(policy|lifecycle|archive|restore)$/.exec(url.pathname);
    if (!matched) throw new Error(`Unexpected ${method} ${url.pathname}`);
    const project = decodeURIComponent(matched[1]!);
    const kind = matched[2]!;
    if (kind === "policy" && method === "GET") {
      if (policyMode.get(project) === "not_stored") {
        return json({ state: "not_stored", project, visibility: null, grants: [], revision: null });
      }
      return json({ state: "ok", project, visibility: "workspace-visible", grants: [], revision: 1 });
    }
    if (kind === "lifecycle" && method === "GET") {
      const status = lifecycleStatus.get(project);
      if (status !== undefined) return json({ error: { code: "unavailable", message: "no lifecycle" } }, status);
      if (lifecycleHold !== null) return await lifecycleHold.promise;
      const row = lifecycles.get(project);
      if (!row) return json({ error: { code: "not_found", message: "no lifecycle" } }, 404);
      return json(row);
    }
    if ((kind === "archive" || kind === "restore") && method === "POST") {
      const body = JSON.parse(String(init?.body)) as { confirmation?: unknown; expected_revision?: unknown };
      archiveSent.push({ project, verb: kind, confirmation: body.confirmation, expected_revision: body.expected_revision });
      if (archiveHold !== null) return await archiveHold.promise;
      if (archiveReply !== null) return archiveReply(project, kind);
      const current = lifecycles.get(project) ?? receipt(project);
      const next = {
        ...current,
        archived: kind === "archive",
        revision: Number(current.revision) + 1,
        changed_at: "2026-04-04T00:00:00Z",
        changed_by: "operator@example.test",
      };
      lifecycles.set(project, next);
      return json(next);
    }
    throw new Error(`Unexpected ${method} ${url.pathname}`);
  }) as typeof fetch;

  /* ------------------------------ test harness ------------------------------ */
  let host = document.createElement("div");
  let root = createRoot(host);
  let toasts: string[] = [];

  async function mount(project: string, block: string | null) {
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
    await act(async () => {
      root.render(React.createElement(LocaleProvider, null,
        React.createElement(ProjectSettings, { project, block, hosted: true, onToast: (text: string) => toasts.push(text) })));
    });
  }
  async function unmount() {
    await act(async () => root.unmount());
    host.remove();
  }
  const button = (text: string, within: ParentNode = host) =>
    Array.from(within.querySelectorAll("button")).find((item) => item.textContent?.trim() === text) as HTMLButtonElement | undefined;
  const dialog = () => host.querySelector<HTMLElement>('[role="dialog"]');
  const click = async (element: Element | undefined) => {
    assert(element, "the control renders");
    await act(async () => (element as HTMLElement).click());
  };
  const typeInto = async (input: HTMLInputElement, value: string) => {
    await act(async () => {
      Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")!.set!.call(input, value);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
  };
  const openArchive = async () => {
    await settle(() => assert(button("Archive project…"), "the archive control renders"));
    await click(button("Archive project…"));
    await settle(() => assert(dialog(), "the exact-key confirm opens"));
    const input = dialog()!.querySelector("input");
    assert(input, "the confirm has its key field");
    return input;
  };

  /* 1. The owner/operator gate: a refused session is offered no transition. */
  lifecycles.set("sample", receipt("sample", { revision: 3 }));
  await mount("sample", "Only the operator can change who can see a project — this session is not the operator's.");
  await settle(() => assert(host.textContent?.includes("Active"), "the proved state renders"));
  assert(!button("Archive project…"), "a non-operator is offered no archive control");
  assert(host.textContent?.includes("this session is not the operator's"), "the refusal is shown");
  await unmount();

  /* 2. Exact key: a mismatch (or a padded key) cannot enable the confirm;
        a matching receipt is the only success. */
  toasts = [];
  lifecycles.set("sample", receipt("sample", { revision: 3 }));
  await mount("sample", null);
  let input = await openArchive();
  assert(document.activeElement === input, "the typed gate takes focus");
  const confirmButton = () => button("Archive project", dialog()!);
  await typeInto(input, "sampl");
  assert(confirmButton()?.disabled, "a partial key keeps confirm off");
  await typeInto(input, " sample");
  assert(confirmButton()?.disabled, "an exact text key is not trimmed into a match");
  await typeInto(input, "sample");
  assert(!confirmButton()?.disabled, "the exact key enables confirm");
  assert(!button("Save policy"), "an open archive confirm locks the policy save");
  await click(confirmButton());
  await settle(() => assert(toasts.includes("Project archived."), "the matching receipt completes the archive"));
  assert(archiveSent.length === 1, "exactly one archive request was sent");
  assert(archiveSent[0]!.confirmation === "sample", "the request carries the exact project key");
  assert(archiveSent[0]!.expected_revision === 3, "the request pins the revision that was read");
  assert(!dialog(), "the confirm closes on a proved receipt");
  await settle(() => assert(!button("Save policy"), "an archived project cannot save a policy"));
  assert(host.textContent?.includes("Archived"), "the panel shows the archived state");
  assert(host.textContent?.includes("This project is archived"), "the archived lock is explained");
  await unmount();

  /* 2b. An unrecorded policy's default proposal is "dirty" before any edit;
        archive is gated on real work, not on that proposal, and the draft
        survives the dialog. */
  toasts = [];
  archiveSent.length = 0;
  policyMode.set("fresh-policy", "not_stored");
  lifecycles.set("fresh-policy", receipt("fresh-policy", { revision: 2 }));
  await mount("fresh-policy", null);
  await settle(() => assert(button("Save policy") && !button("Save policy")!.disabled, "the first-record proposal is enabled"));
  assert(button("Archive project…") && !button("Archive project…")!.disabled, "the dirty default proposal does not block archive");
  input = await openArchive();
  await click(button("Cancel", dialog()!));
  await settle(() => assert(!dialog(), "the confirm closes without a request"));
  assert(button("Save policy") && !button("Save policy")!.disabled, "the unsaved first-record draft survives the dialog");
  input = await openArchive();
  await typeInto(input, "fresh-policy");
  await click(button("Archive project", dialog()!));
  await settle(() => assert(toasts.includes("Project archived."), "archive completes for an unrecorded policy"));
  await unmount();

  /* 2c. A cached active receipt does not unlock while it is being
        revalidated: another tab may have archived the project. */
  lifecycles.set("recheck", receipt("recheck", { revision: 1 }));
  await mount("recheck", null);
  await settle(() => assert(button("Save policy"), "the current active answer unlocks policy"));
  const recheckHeld = deferred.withResolvers<Response>();
  lifecycleHold = { promise: recheckHeld.promise, resolve: recheckHeld.resolve };
  await act(async () => {
    void resources.projectLifecycle("recheck").invalidate();
  });
  await settle(() => assert(!button("Save policy"), "a revalidating lifecycle locks policy"));
  assert(button("Archive project…")?.disabled, "a revalidating lifecycle blocks transitions");
  await act(async () => recheckHeld.resolve(json(lifecycles.get("recheck"))));
  await settle(() => assert(button("Save policy"), "the current active answer unlocks policy again"));
  await unmount();
  lifecycleHold = null;

  /* 3. A receipt at the pinned revision is stale: no toast, no close. */
  toasts = [];
  archiveSent.length = 0;
  lifecycles.set("stale-receipt", receipt("stale-receipt", { revision: 5 }));
  archiveReply = (project) => json(receipt(project, { revision: 5, archived: false }));
  await mount("stale-receipt", null);
  input = await openArchive();
  await typeInto(input, "stale-receipt");
  await click(button("Archive project", dialog()!));
  await settle(() => assert(dialog()?.textContent?.includes("without a receipt that proves this change"), "the stale answer is reported"));
  assert(toasts.length === 0, "a stale receipt reports no success");
  assert(dialog(), "the confirm stays open after a stale receipt");
  // A jump may belong to another actor's transition: it is not this success.
  archiveReply = (project) => json(receipt(project, { revision: 7, archived: true }));
  await click(button("Archive project", dialog()!));
  await settle(() => assert(dialog()?.textContent?.includes("without a receipt that proves this change"), "a jumped revision is refused"));
  assert(toasts.length === 0, "a jumped receipt reports no success");
  await unmount();
  archiveReply = null;

  /* 4. A reply that belongs to a project the page has since left is dropped. */
  toasts = [];
  archiveSent.length = 0;
  lifecycles.set("proj-a", receipt("proj-a", { revision: 7 }));
  lifecycles.set("proj-b", receipt("proj-b", { revision: 1 }));
  await mount("proj-a", null);
  input = await openArchive();
  await typeInto(input, "proj-a");
  const held = deferred.withResolvers<Response>();
  archiveHold = { promise: held.promise, resolve: held.resolve };
  await click(button("Archive project", dialog()!));
  await settle(() => assert(archiveSent.length === 1, "the archive request is pending"));
  // The person moves to another project while the write is unanswered.
  await act(async () => {
    root.render(React.createElement(LocaleProvider, null,
      React.createElement(ProjectSettings, { project: "proj-b", block: null, hosted: true, onToast: (text: string) => toasts.push(text) })));
  });
  await act(async () => held.resolve(json(receipt("proj-a", { revision: 8, archived: true }))));
  await act(async () => {
    await held.promise;
    await Promise.resolve();
  });
  assert(toasts.length === 0, "the stale project's reply reports no success");
  assert(!dialog(), "the retired project's confirm was closed by the route change");
  await settle(() => assert(host.textContent?.includes("proj-b"), "the new project is on screen"));
  await unmount();
  archiveHold = null;

  /* 5. A private-session change retires the continuation: no success. */
  toasts = [];
  archiveSent.length = 0;
  lifecycles.set("sess-a", receipt("sess-a", { revision: 2 }));
  await mount("sess-a", null);
  input = await openArchive();
  await typeInto(input, "sess-a");
  const sessionHeld = deferred.withResolvers<Response>();
  archiveHold = { promise: sessionHeld.promise, resolve: sessionHeld.resolve };
  await click(button("Archive project", dialog()!));
  await settle(() => assert(archiveSent.length === 1, "the archive request is pending"));
  await act(async () => {
    session.resetPrivateSession();
    session.allowPrivateReads();
  });
  await act(async () => sessionHeld.resolve(json(receipt("sess-a", { revision: 3, archived: true }))));
  await act(async () => {
    await sessionHeld.promise.catch(() => undefined);
    await Promise.resolve();
  });
  assert(toasts.length === 0, "a reply from a retired session reports no success");
  await unmount();
  archiveHold = null;

  /* 6. An older server proves only that it lacks the endpoint: unavailable
        messaging, no transition, and policy stays read-only (unverified is
        not active, including after a rollback). */
  lifecycleStatus.set("legacy", 501);
  await mount("legacy", null);
  await settle(() => assert(host.textContent?.includes("Project archive is unavailable on this server"), "the capability is called unavailable"));
  assert(!button("Save policy"), "an unsupported server never unlocks access changes");
  assert(
    host.textContent?.includes("does not serve the project archive contract"),
    "the locked reason names the missing backend contract",
  );
  assert(!button("Archive project…"), "an unavailable capability offers no archive control");
  assert(!host.textContent?.includes("Project archived."), "an unavailable server never reports an archive");
  assert(host.textContent?.includes("Visibility"), "the policy page still shows the record read-only");
  await unmount();

  /* 7. Server blockers are displayed, and archive is not offered past them. */
  lifecycles.set("blocked-work", receipt("blocked-work", { revision: 4, blockers: ["Open work must be closed first"] }));
  await mount("blocked-work", null);
  await settle(() => assert(host.textContent?.includes("Open work must be closed first"), "the server's blockers are shown"));
  assert(button("Archive project…")?.disabled, "blocked archive is not offered");
  await unmount();

  /* 8. An unanswered lifecycle route (404) is unknown, not "unavailable":
        policy mutation stays locked and no transition is offered. */
  lifecycleStatus.set("unknown-state", 404);
  await mount("unknown-state", null);
  await settle(() => assert(host.textContent?.includes("could not be proved"), "the unknown state is explicit"));
  assert(!button("Save policy"), "an unknown archive state locks policy mutation");
  assert(!button("Archive project…"), "an unknown archive state offers no transition");
  assert(!host.textContent?.includes("Project archive is unavailable"), "a 404 is not reported as an unavailable capability");
  await unmount();

  /* 9. A receipt the server cannot emit (revision > 0 with null evidence) is
        unreadable: no state, no transition, policy locked. */
  lifecycles.set("malformed", receipt("malformed", {
    revision: 3,
    archived_at: null,
    archived_by: null,
    changed_at: null,
    changed_by: null,
  }));
  await mount("malformed", null);
  await settle(() => assert(host.textContent?.includes("cannot prove"), "a malformed receipt is named unreadable"));
  assert(!button("Save policy"), "a malformed receipt locks policy mutation");
  assert(!button("Archive project…"), "a malformed receipt offers no transition");
  await unmount();

  /* 10. Genuine unsaved policy edits block archive; the untouched default
        proposal does not (covered above). */
  lifecycles.set("edited-policy", receipt("edited-policy", { revision: 6 }));
  await mount("edited-policy", null);
  await settle(() => assert(button("Archive project…") && !button("Archive project…")!.disabled, "the archive control is enabled"));
  const privateChoice = Array.from(host.querySelectorAll<HTMLButtonElement>("button.ps-visibility"))
    .find((item) => item.textContent?.includes("Private"));
  assert(privateChoice, "the visibility choice renders");
  await click(privateChoice);
  await settle(() => assert(button("Archive project…")?.disabled, "unsaved policy edits block archive"));
  assert(host.textContent?.includes("Save or discard your access changes"), "the unsaved-edit block is explained");
  await click(button("Discard"));
  await settle(() => assert(!button("Archive project…")?.disabled, "discarding the edit re-enables archive"));
  await unmount();

  /* 11. Numeric confirmation keeps trimming a code — the default is unchanged. */
  {
    const numericHost = document.createElement("div");
    document.body.append(numericHost);
    const numericRoot = createRoot(numericHost);
    await act(async () => numericRoot.render(React.createElement(LocaleProvider, null,
      React.createElement(ConfirmDialog, {
        title: "Confirm",
        body: "Body",
        expect: "7",
        match: "7",
        inputLabel: "Code",
        confirmLabel: "Delete",
        pending: false,
        error: null,
        onConfirm: () => undefined,
        onCancel: () => undefined,
      }))));
    await settle(() => assert(numericHost.querySelector("input"), "the numeric gate renders"));
    const field = numericHost.querySelector<HTMLInputElement>("input")!;
    const confirm = button("Delete", numericHost);
    await typeInto(field, " 7 ");
    assert(confirm && !confirm.disabled, "a numeric code still ignores surrounding whitespace");
    await act(async () => numericRoot.unmount());
    numericHost.remove();
  }

  // The panel's unavailable path asked the lifecycle route; the retry-able
  // read for a served project is covered above.
  /* 12. The archived-project issue read opts in explicitly; an older server
        rejects it instead of answering the ordinary project list. */
  {
    let asked: string | null = null;
    const previousFetch = globalThis.fetch;
    globalThis.fetch = (async (input: unknown) => {
      const url = new URL(typeof input === "string" ? input : String((input as Request).url), "http://localhost");
      asked = `${url.pathname}${url.search}`;
      return json({ issues: [] });
    }) as typeof fetch;
    const { api } = require("../src/lib/api") as typeof ApiModule;
    await api.archivedProjectIssues("legacy-app");
    assert(
      asked === "/api/issues?project=legacy-app&archived=1",
      "the archived project read pins the opt-in archived query",
    );
    globalThis.fetch = previousFetch;
  }

  console.log("project archive panel checks passed");
}

void main().catch((error: unknown) => {
  console.error(error);
  throw error;
});
