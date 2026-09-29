import assert from "node:assert/strict";
import { test } from "node:test";
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import { resolve } from "node:path";
const repo = fileURLToPath(new URL("../../", import.meta.url));
const tools = createRequire(resolve(repo, "ui/package.json"));

test("publish preview gates post-now/schedule on exact approval and covers all dispatch states without a live send", async () => {
  const { Window } = tools("happy-dom");
  const window = new Window();
  globalThis.window = window;
  globalThis.document = window.document;
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  const React = tools("react"),
    { createRoot } = tools("react-dom/client"),
    ts = tools("typescript");
  const icons = new Map();
  for (const name of [
    "CubeIcon",
    "File01Icon",
    "Tick02Icon",
    "MailSend01Icon",
    "ContrastIcon",
  ]) {
    const id = `@hugeicons/core-free-icons/${name}`;
    icons.set(
      id,
      await import(
        pathToFileURL(tools.resolve(id).replace("/dist/cjs/", "/dist/esm/"))
          .href
      ),
    );
  }
  function load(file) {
    const local = createRequire(file),
      exports = {};
    const code = ts.transpileModule(
      readFileSync(file, "utf8").replaceAll(
        "__APP_DEV_REVISION__",
        '"test-base"',
      ),
      {
        compilerOptions: {
          jsx: ts.JsxEmit.ReactJSX,
          module: ts.ModuleKind.CommonJS,
          target: ts.ScriptTarget.ES2022,
        },
      },
    ).outputText;
    new Function("require", "exports", code)(
      (id) =>
        icons.has(id)
          ? icons.get(id)
          : id.startsWith(".")
            ? id.endsWith(".jsx")
              ? load(resolve(file, "..", id))
              : local(id)
            : tools(id),
      exports,
    );
    return exports;
  }
  const View = load(
    resolve(repo, "app-previews/social-content/SocialContent.jsx"),
  ).default;
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const act = React.act;
  const button = (text) =>
    [...host.querySelectorAll("button")].find(
      (value) => value.textContent.trim() === text,
    );
  async function click(text) {
    const target = button(text);
    assert.ok(target, text);
    assert.ok(!target.disabled, text);
    await act(() => target.click());
    return target;
  }
  try {
    await act(() => root.render(React.createElement(View)));
    await click("Publish");
    assert.match(host.textContent, /@sakeboyhk/);
    assert.match(host.textContent, /caption digest/);
    assert.match(host.textContent, /media digest/);
    assert.match(host.textContent, /binding digest/);
    assert.match(host.textContent, /idempotency key/);
    assert.match(host.textContent, /HK\$12/);
    assert.match(host.textContent, /1–10 uses/);
    assert.match(host.textContent, /refusal code, not a state/);
    assert.match(host.textContent, /Fixtures only/);
    // No live-send affordance: every enabled send-flavoured button is a
    // simulation; disabled gallery buttons are labelled fixture-only.
    for (const value of [...host.querySelectorAll("button")]) {
      const text = value.textContent.trim();
      if (!/Post now|Schedule|Reconnect|Cancel/.test(text)) continue;
      if (value.disabled)
        assert.match(value.title || value.getAttribute("aria-label") || "", /Fixture|approv/i, text);
      else assert.match(text, /Simulate|Back to review/, text);
    }
    // Post now is gated on exact approval.
    assert.ok(button("Simulate Post now").disabled);
    const approve = host.querySelector(
      'input[aria-label="Approve the exact destination and content digests"]',
    );
    assert.ok(approve);
    await act(() => approve.click());
    assert.ok(!button("Simulate Post now").disabled);
    await click("Simulate Post now");
    assert.match(host.textContent, /in flight/);
    await click("Simulate posted");
    assert.match(host.textContent, /verified receipt/);
    assert.match(host.textContent, /permalink/);
    await click("Back to review");
    assert.ok(button("Simulate Post now").disabled);
    // Schedule path: queued with due time, then cancelled.
    const scheduleRadio = host.querySelector('input[value="schedule"]');
    await act(() => scheduleRadio.click());
    await act(() => approve.click());
    const scheduleButton = button("Simulate Schedule");
    assert.ok(scheduleButton && !scheduleButton.disabled);
    await click("Simulate Schedule");
    assert.match(host.textContent, /Simulated queued/);
    assert.match(host.textContent, /Asia\/Hong_Kong/);
    await click("Simulate cancel");
    assert.match(host.textContent, /nothing was sent/i);
    await click("Back to review");
    // Authority change holds for reconnect and requires fresh approval.
    await act(() => approve.click());
    await click("Simulate authority change");
    assert.match(host.textContent, /Held/);
    await click("Simulate reconnect");
    assert.match(host.textContent, /re-approve/);
    assert.ok(button("Simulate Schedule").disabled);
    // Lost response reads as uncertain while state stays processing.
    const nowRadio = host.querySelector('input[value="now"]');
    await act(() => nowRadio.click());
    await act(() => approve.click());
    await click("Simulate Post now");
    await click("Simulate lost response");
    assert.match(host.textContent, /reads as uncertain/);
    assert.match(host.textContent, /reconcile/i);
    // Every required dispatch state is illustrated.
    for (const id of [
      "queued",
      "processing",
      "posted",
      "refused",
      "reading",
      "cancelled",
      "held",
    ]) {
      assert.ok(
        [...host.querySelectorAll(".badge")].some(
          (badge) => badge.textContent.trim() === id,
        ),
        id,
      );
    }
  } finally {
    await act(() => root.unmount());
    host.remove();
  }
});
