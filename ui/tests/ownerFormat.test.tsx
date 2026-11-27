/** CAD-2878: pure helpers and Fields owner picker tests. */
declare function require<T = unknown>(name: string): T;
declare const process: { exitCode?: number };

import type * as ReactModule from "react";
import type { Root } from "react-dom/client";
import type * as OwnerFormatModule from "../src/features/issues/ownerFormat";
import type * as FieldsModule from "../src/features/issues/Fields";
import type * as LocaleModule from "../src/lib/locale";
import type { AgentProfile } from "../src/lib/generated/board-api";
import type { IssueDetail } from "../src/lib/types";
import type { HumanPerson } from "../src/features/threads/humanThreadsClient";

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}

async function main(): Promise<void> {
  const { Window } = require<{ Window: new (options: { url: string }) => Record<string, any> }>(
    "happy-dom",
  );
  const win = new Window({ url: "http://localhost/projects/demo/issues/CAD-1" });
  const pointerCtor = win.PointerEvent ?? win.MouseEvent;
  for (const name of [
    "window",
    "document",
    "Node",
    "Element",
    "HTMLElement",
    "HTMLButtonElement",
    "HTMLInputElement",
    "SVGElement",
    "navigator",
    "MutationObserver",
    "ResizeObserver",
    "Event",
    "MouseEvent",
    "KeyboardEvent",
    "location",
    "history",
    "sessionStorage",
    "localStorage",
  ]) {
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  }
  Object.defineProperty(globalThis, "PointerEvent", {
    value: pointerCtor,
    configurable: true,
    writable: true,
  });
  for (const name of ["addEventListener", "removeEventListener"]) {
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  }
  Object.defineProperty(globalThis, "requestAnimationFrame", {
    value: (cb: (time: number) => void) => setTimeout(() => cb(Date.now()), 0),
    configurable: true,
  });
  Object.defineProperty(globalThis, "cancelAnimationFrame", {
    value: (id: ReturnType<typeof setTimeout>) => clearTimeout(id),
    configurable: true,
  });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true, configurable: true });
  win.HTMLElement.prototype.scrollIntoView = () => {};

  const loader = require<{ prototype: { require: (this: unknown, id: string) => unknown } }>(
    "module",
  );
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.call(this, id);
  };

  const React = require<typeof ReactModule>("react");
  const { createRoot } = require<{ createRoot: (host: Element) => Root }>("react-dom/client");
  const { formatTicketOwner, buildOwnerSelectOptions } = require<typeof OwnerFormatModule>(
    "../src/features/issues/ownerFormat",
  );
  const { Fields } = require<typeof FieldsModule>("../src/features/issues/Fields");
  const { LocaleProvider } = require<typeof LocaleModule>("../src/lib/locale");

  const profiles: AgentProfile[] = [
    {
      id: "ap_research",
      alias: "research-agent",
      role: "Researcher",
      model: "inherit",
      runtimes: [],
      team_role: "worker",
    },
    {
      id: "ap_reviewer",
      alias: "Reviewer",
      role: "reviewer",
      model: "inherit",
      runtimes: [],
      team_role: "reviewer",
    },
  ];

  const people: Array<HumanPerson & { handle?: string }> = [
    { id: "hum_1", subject: "usr_1", name: "Chris Chiang", handle: "chris-chiang" },
    { id: "hum_2", subject: "usr_2", name: "Patrick Wun", handle: "patrick-wun" },
  ];

  equal(formatTicketOwner(null, profiles), null, "null owner returns null");
  equal(formatTicketOwner("", profiles), null, "empty owner returns null");
  equal(
    formatTicketOwner("agents/ap_research", profiles),
    "@research-agent",
    "formats known agent owner as @alias",
  );
  equal(
    formatTicketOwner("agents/ap_unknown", profiles),
    "agents/ap_unknown",
    "falls back to raw string when profile is unknown",
  );
  equal(formatTicketOwner("@patrick-wun", profiles), "@patrick-wun", "keeps human @handle");

  const opts = buildOwnerSelectOptions({
    currentOwner: "legacy-custom",
    people,
    profiles,
    t: (s: string) => s,
  });
  equal(opts[0], { value: "", label: "Unassigned" }, "starts with Unassigned");
  equal(
    opts[1],
    { value: "legacy-custom", label: "legacy-custom" },
    "preserves unmatched legacy currentOwner",
  );
  assert(
    opts.some((o) => o.value === "@chris-chiang" && o.group === "Teammates"),
    "includes self/teammate handle",
  );
  assert(
    opts.some((o) => o.value === "@patrick-wun" && o.hint === "Patrick Wun"),
    "includes teammate handle and name hint",
  );
  assert(
    opts.some((o) => o.value === "agents/ap_research" && o.label === "@research-agent"),
    "includes shareable cloud agent option",
  );
  assert(!opts.some((o) => o.value === "agents/ap_reviewer"), "excludes reviewer profile");

  const baseDetail: IssueDetail = {
    id: "CAD-1",
    project: "demo",
    title: "Test ticket",
    status: "ready",
    status_source: "file",
    priority: "P1",
    owner: null,
    claim: null,
    component: null,
    tags: [],
    paths: [],
    parent: null,
    blocked_by: [],
    relates: [],
    duplicate_of: null,
    refs: [],
    container: false,
    ready: true,
    blocked: false,
    blocked_reason: null,
    created: "2026-11-27T00:00:00Z",
    rev: "rev_1",
    checks: { done: 0, total: 0 },
    work: { type: "task", type_source: "implicit", weight: 1 },
    frontmatter: {},
    body: "",
    path: "",
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
  };

  const patches: Array<{ owner?: string }> = [];
  const host = win.document.createElement("div");
  win.document.body.appendChild(host);
  const root = createRoot(host);

  const flush = () => new Promise((resolve) => setTimeout(resolve, 25));

  root.render(
    React.createElement(
      LocaleProvider,
      null,
      React.createElement(Fields, {
        detail: baseDetail,
        epics: [],
        readOnly: false,
        hosted: true,
        people,
        profiles,
        onWrite: () => undefined,
        onError: () => undefined,
        onPatch: (body: { owner?: string }) => {
          patches.push(body);
        },
      }),
    ),
  );
  await flush();

  const trigger = host.querySelector('button[aria-label="Owner"]') as HTMLElement | null;
  assert(trigger !== null, "hosted Fields renders searchable Select button for Owner");

  trigger.dispatchEvent(new win.MouseEvent("click", { bubbles: true }));
  await flush();

  const pickOption = (labelText: string) => {
    const items = Array.from(win.document.querySelectorAll('[role="option"]')) as HTMLElement[];
    const match = items.find((el) => el.textContent?.includes(labelText));
    assert(match, `expected option containing ${labelText}`);
    match.dispatchEvent(new win.MouseEvent("click", { bubbles: true }));
  };

  pickOption("@patrick-wun");
  await flush();
  equal(patches[0]?.owner, "@patrick-wun", "selecting human sends @handle");

  trigger.dispatchEvent(new win.MouseEvent("click", { bubbles: true }));
  await flush();
  pickOption("@research-agent");
  await flush();
  equal(patches[1]?.owner, "agents/ap_research", "selecting agent sends agents/<profile_id>");

  // Rerender with non-null owner so picking Unassigned emits empty string
  root.render(
    React.createElement(
      LocaleProvider,
      null,
      React.createElement(Fields, {
        detail: { ...baseDetail, owner: "@patrick-wun", rev: "rev_2" },
        epics: [],
        readOnly: false,
        hosted: true,
        people,
        profiles,
        onWrite: () => undefined,
        onError: () => undefined,
        onPatch: (body: { owner?: string }) => {
          patches.push(body);
        },
      }),
    ),
  );
  await flush();

  const trigger2 = host.querySelector('button[aria-label="Owner"]') as HTMLElement;
  trigger2.dispatchEvent(new win.MouseEvent("click", { bubbles: true }));
  await flush();
  pickOption("Unassigned");
  await flush();
  equal(patches[2]?.owner, "", "selecting Unassigned clears owner");

  // Native fallback without hosted directory keeps text input
  root.render(
    React.createElement(
      LocaleProvider,
      null,
      React.createElement(Fields, {
        detail: baseDetail,
        epics: [],
        readOnly: false,
        hosted: false,
        people: [],
        profiles: [],
        onWrite: () => undefined,
        onError: () => undefined,
        onPatch: () => undefined,
      }),
    ),
  );
  await flush();
  const nativeInput = host.querySelector('input[aria-label="Owner"]');
  assert(nativeInput !== null, "native fallback renders text input");

  root.unmount();
  host.remove();
  win.close();
}

void main().catch((err: unknown) => {
  console.error(err);
  process.exitCode = 1;
});
