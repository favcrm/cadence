import assert from "node:assert/strict";
import { test } from "node:test";
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { resolve } from "node:path";
const repo = fileURLToPath(new URL("../../", import.meta.url));
const tools = createRequire(resolve(repo, "ui/package.json"));
const source = createRequire(
  resolve(repo, "app-previews/social-content/SocialContent.jsx"),
);
test("actual React view mounts and local draft flow works without a backend", async () => {
  const { Window } = tools("happy-dom"),
    window = new Window();
  globalThis.window = window;
  globalThis.document = window.document;
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  const React = tools("react"),
    { createRoot } = tools("react-dom/client"),
    ts = tools("typescript");
  const code = ts.transpileModule(
    readFileSync(
      resolve(repo, "app-previews/social-content/SocialContent.jsx"),
      "utf8",
    ).replaceAll("__APP_DEV_REVISION__", '"test-base"'),
    {
      compilerOptions: {
        jsx: ts.JsxEmit.ReactJSX,
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    },
  ).outputText;
  const exports = {};
  new Function("require", "exports", code)(
    (id) => (id.startsWith(".") ? source(id) : tools(id)),
    exports,
  );
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const act = React.act;
  const click = async (text) => {
    const button = [...host.querySelectorAll("button")].find(
      (b) => b.textContent.trim() === text,
    );
    assert.ok(button, text);
    assert.ok(!button.disabled, text);
    await act(() => button.click());
  };
  try {
    await act(() => root.render(React.createElement(exports.default)));
    assert.ok(host.textContent.includes("Fixtures only"));
    assert.ok(host.textContent.includes("Harbour studio"));
    const { sdk } = source("./store.mjs");
    // The same app facade creates data; Reset fixtures reconciles the mounted view.
    await act(() => {
      host
        .querySelector("form")
        .dispatchEvent(
          new window.Event("submit", { bubbles: true, cancelable: true }),
        );
    });
    assert.ok(
      host.textContent.includes("Add source text before creating a draft."),
    );
    await click("Mark reviewed");
    await click("Stage locally");
    assert.ok(host.textContent.includes("Nothing published."));
    assert.equal(sdk.read()[0].status, "local-outbox");
    await click("Reset fixtures");
    assert.equal(sdk.read()[0].status, "draft");
    const select = host.querySelector("#scenario");
    await act(() => {
      select.value = "readonly";
      select.dispatchEvent(new window.Event("change", { bubbles: true }));
    });
    assert.ok(host.textContent.includes("Read-only preview"));
    assert.ok(host.querySelector("#caption").disabled);
  } finally {
    await act(() => root.unmount());
    await window.happyDOM.close();
  }
});
