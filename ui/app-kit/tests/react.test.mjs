import { test, afterEach } from "node:test";
import assert from "node:assert/strict";
import { Window } from "happy-dom";
import React, { createElement } from "react";
import { createRoot } from "react-dom/client";
import { createScreenBridge } from "../src/index.mjs";
import {
  createFrameNavigationAdapter,
  useScreenBridge,
  useScreenSelector,
  useScreenStatus,
} from "../src/react.mjs";

const boot = {
  parent_origin: "https://board.example",
  bridge_nonce: "a".repeat(64),
  generation: 1,
  tag: "notes",
};

const cleanupTasks = [];

afterEach(() => {
  while (cleanupTasks.length > 0) {
    const fn = cleanupTasks.pop();
    try {
      fn();
    } catch {
      // best-effort teardown
    }
  }
});

function tick() {
  return new Promise((resolve) => setTimeout(resolve, 10));
}

async function waitFor(find, label) {
  for (let i = 0; i < 100; i += 1) {
    const val = find();
    if (val) return val;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  throw new Error(`timed out waiting for ${label}`);
}

function setupDom() {
  const win = new Window({ url: "https://frame.local/" });
  const prev = new Map();
  const keys = ["window", "document", "Node", "Element", "HTMLElement", "IS_REACT_ACT_ENVIRONMENT"];
  for (const k of keys) {
    prev.set(k, Object.getOwnPropertyDescriptor(globalThis, k));
  }
  for (const k of ["window", "document", "Node", "Element", "HTMLElement"]) {
    Object.defineProperty(globalThis, k, {
      value: k === "window" ? win : win[k],
      configurable: true,
      writable: true,
    });
  }
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", {
    value: true,
    configurable: true,
    writable: true,
  });
  cleanupTasks.push(() => {
    for (const [k, desc] of prev.entries()) {
      if (desc) Object.defineProperty(globalThis, k, desc);
      else delete globalThis[k];
    }
    win.close();
  });
  return { win, doc: win.document };
}

function startBridge(bridge) {
  let host;
  const inbox = [];
  const parent = {
    postMessage(_msg, _origin, transfer) {
      host = transfer?.[0];
      if (host) host.onmessage = (event) => inbox.push(event.data);
    },
  };
  bridge.start({ boot, parent });
  cleanupTasks.push(() => bridge.reset());
  return { host, inbox };
}

test("@cadence/app-kit/react hooks subscribe, memoize status/selectors, and drive frame navigation", async () => {
  const { doc } = setupDom();
  const bridge = createScreenBridge({
    tag: "notes",
    appName: "notes",
    records: true,
    surfaces: true,
    locale: true,
    calls: { "screen.route.patch": ["section", "view", "record_id", "segment_id"] },
    route: { sections: ["all", "open"], defaultSection: "all", views: ["list", "new", "board"] },
  });
  const { host, inbox } = startBridge(bridge);
  await tick();

  const seenStatuses = [];
  const seenSections = [];

  const installation = {
    install_id: "install-1",
    name: "notes",
    title: "Notes",
    version: "0.1.0",
    digest: "sha256:" + "b".repeat(64),
    summary: "Notes",
  };

  function App() {
    const state = useScreenBridge(bridge);
    const status = useScreenStatus(bridge);
    const section = useScreenSelector(bridge, (s) => s.snapshot?.route?.section ?? null);
    seenStatuses.push(status);
    seenSections.push(section);
    return createElement(
      "div",
      { "data-status": status.status, "data-section": section ?? "none", "data-state": state.status },
      `${status.status}:${section ?? "none"}`,
    );
  }

  const container = doc.createElement("div");
  doc.body.append(container);
  const root = createRoot(container);

  try {
    await React.act(async () => {
      root.render(createElement(App));
    });
    assert.equal(container.textContent, "loading:none");
    assert.equal(seenStatuses[0].isLoading, true);
    assert.equal(seenStatuses[0].ready, false);

    await React.act(async () => {
      host.postMessage({ v: 1, op: "surface", role: "primary", modal: false });
      host.postMessage({
        v: 2,
        op: "screen",
        tag: "notes",
        install_id: "install-1",
        digest: "sha256:" + "b".repeat(64),
        context_id: "ctx-1",
        now: 1,
        installation,
        actions: ["screen.route.patch"],
        presentation_locale: "en",
        route: { section: "all", view: "list", record_id: null, segment_id: null },
      });
      await waitFor(() => bridge.getState().status === "ready", "ready");
    });

    assert.equal(container.textContent, "ready:all");
    assert.equal(seenStatuses.at(-1).ready, true);
    assert.equal(seenStatuses.at(-1).isError, false);

    const nav = createFrameNavigationAdapter(bridge);
    assert.equal(nav.modifiedClicks, "prevent");
    assert.equal(nav.href(""), "#");
    assert.equal(nav.href("?section=open&view=new&record_id=rec_2"), "?section=open&view=new&record_id=rec_2");

    const statusBeforeRoute = seenStatuses.at(-1);
    nav.navigate("?section=open&view=new&record_id=rec_2");
    const patchMsg = await waitFor(() => inbox.find((m) => m.op === "call"), "route patch call");
    assert.equal(patchMsg.verb, "screen.route.patch");
    assert.deepEqual(patchMsg.args, { section: "open", view: "new", record_id: "rec_2" });

    await React.act(async () => {
      host.postMessage({
        v: 2,
        op: "reply",
        id: patchMsg.id,
        ok: true,
        data: { section: "open", view: "new", record_id: "rec_2", segment_id: null },
      });
      host.postMessage({
        v: 2,
        op: "screen",
        tag: "notes",
        install_id: "install-1",
        digest: "sha256:" + "b".repeat(64),
        context_id: "ctx-1",
        now: 2,
        installation,
        actions: ["screen.route.patch"],
        presentation_locale: "en",
        route: { section: "open", view: "new", record_id: "rec_2", segment_id: null },
      });
      await waitFor(() => bridge.getState().snapshot?.route?.section === "open", "route updated");
    });

    assert.equal(container.textContent, "ready:open");
    assert.equal(seenSections.at(-1), "open");
    assert.equal(seenStatuses.at(-1), statusBeforeRoute);

    // Empty section/view params are omitted rather than emitted as null, while record_id/segment_id clear to null.
    nav.navigate("?section=&view=&record_id=");
    const clearPatchMsg = await waitFor(
      () => inbox.filter((m) => m.op === "call" && m.verb === "screen.route.patch")[1],
      "empty query patch",
    );
    assert.deepEqual(clearPatchMsg.args, { record_id: null });

    // navOptions.replace forwards replace boolean even if app's declared calls["screen.route.patch"] omitted "replace".
    nav.navigate("?section=all", { replace: true });
    const replacePatchMsg = await waitFor(
      () => inbox.filter((m) => m.op === "call" && m.verb === "screen.route.patch")[2],
      "replace patch",
    );
    assert.deepEqual(replacePatchMsg.args, { section: "all", replace: true });

    // Custom declared route views pass through instead of being hardcoded to "list" | "new".
    nav.navigate("?view=board");
    const customViewMsg = await waitFor(
      () => inbox.filter((m) => m.op === "call" && m.verb === "screen.route.patch")[3],
      "custom view patch",
    );
    assert.deepEqual(customViewMsg.args, { view: "board" });
  } finally {
    await React.act(async () => {
      root.unmount();
    });
  }
});

test("useScreenSelector resets cache cleanly when bridge prop changes", async () => {
  const { doc } = setupDom();
  const makeFakeBridge = (value) => {
    const subs = new Set();
    let cur = value;
    return {
      subscribe: (fn) => { subs.add(fn); return () => subs.delete(fn); },
      getState: () => ({ status: "ready", snapshot: { tag: cur } }),
      set(next) { cur = next; for (const fn of subs) fn(); },
    };
  };
  const b1 = makeFakeBridge("one");
  const b2 = makeFakeBridge("two");
  const sel = (s) => s.snapshot?.tag;
  function Probe({ bridge }) {
    const tagVal = useScreenSelector(bridge, sel);
    return createElement("span", null, tagVal);
  }
  const container = doc.createElement("div");
  doc.body.append(container);
  const root = createRoot(container);
  try {
    await React.act(async () => { root.render(createElement(Probe, { bridge: b1 })); });
    assert.equal(container.textContent, "one");
    await React.act(async () => { root.render(createElement(Probe, { bridge: b2 })); });
    assert.equal(container.textContent, "two");
  } finally {
    await React.act(async () => { root.unmount(); });
  }
});
