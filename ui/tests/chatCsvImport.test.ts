/*
 * CAD-1016 mounted chat CSV import checks. These mount ChatCsvImport
 * against a stubbed fetch + a stub `onSendIntent`, and drive the real
 * flow: paste → preview → review rows → Confirm. The confirm stores the
 * durable plan (csv_text + decisions + digest) and the scoped message
 * carries only the {request_id, confirm_token} receipt — never the CSV.
 * Assertions cover: the confirm body carries the durable plan fields;
 * the intent send carries ONLY request_id+confirm_token (no csv_text,
 * decisions or preview_token on the wire); a cancel writes nothing; a
 * refused confirm starts no turn; a send failure surfaces honestly;
 * and a double-click mints/sends at most once.
 */

declare function require(name: string): any;
import type { act as reactAct } from "react";
import type { HostScope } from "../src/features/app-shell/hostActions";

function assert(cond: unknown, why: string): asserts cond {
  if (!cond) throw new Error(why);
}
function equal(actual: unknown, expected: unknown, why: string): void {
  if (actual !== expected) {
    throw new Error(`${why}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

let act: typeof reactAct;
async function settle(fn: () => void, ms = 12000) {
  const end = Date.now() + ms;
  for (;;) {
    try {
      fn();
      return;
    } catch (e) {
      if (Date.now() > end) throw e;
      await act(async () => sleep(25));
    }
  }
}

const CSV = "record_id,display_name,email\nrec-1,Ada,ada@example.com\n";

// A preview receipt: one create row, no decisions needed.
function previewReceipt(): Response {
  return new Response(
    JSON.stringify({
      preview_token: "sha256:" + "a".repeat(64),
      row_count: 1,
      summary: { create: 1, update: 0, skip: 0, needs_revision: 0, error: 0 },
      rows: [
        {
          row: 1, record_id: "rec-1", decision: "create", expected_revision: null,
          current_revision: null, profile: { display_name: "Ada" }, errors: [], reason: null, duplicate_of: null,
        },
      ],
    }),
    { status: 200, headers: { "Content-Type": "application/json" } },
  );
}

const SCOPE: HostScope = { installId: "install-crm", contextId: "ctx-a" };

async function mount(opts: {
  previewResponse?: () => Response;
  confirmResponse?: () => Response;
  sendIntent?: (intent: any) => Promise<string | null>;
}) {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/app/crm" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLSelectElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener"])
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  const loader = require("module"), originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, arguments);
  };

  const React = require("react") as typeof import("react");
  act = React.act;
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const { default: ChatCsvImport } = require("../src/features/app-shell/ChatCsvImport") as typeof import("../src/features/app-shell/ChatCsvImport");

  const host = document.createElement("div");
  document.body.appendChild(host);
  const text = () => host.textContent ?? "";
  const byText = (tag: string, label: string) =>
    Array.from(host.querySelectorAll(tag)).find((el) => (el.textContent ?? "").includes(label)) ?? null;

  const posts: { path: string; body: any }[] = [];
  const sentIntents: any[] = [];
  let sendError: string | null = null;
  const innerFetch = globalThis.fetch;
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = String(input);
    const pathname = new URL(url, "http://localhost").pathname;
    const body = init?.body ? JSON.parse(String(init.body)) : null;
    if (init?.method === "POST" && pathname.endsWith("/records/csv-preview")) {
      posts.push({ path: "csv-preview", body });
      return opts.previewResponse ? opts.previewResponse() : previewReceipt();
    }
    if (init?.method === "POST" && pathname.endsWith("/records/csv-confirm")) {
      posts.push({ path: "csv-confirm", body });
      return opts.confirmResponse
        ? opts.confirmResponse()
        : new Response(
            JSON.stringify({ confirm_token: "confirm-abc123", state: "open", request_id: body.request_id }),
            { status: 200, headers: { "Content-Type": "application/json" } },
          );
    }
    return innerFetch(input as RequestInfo | URL, init);
  }) as typeof fetch;

  const sendIntent =
    opts.sendIntent ??
    (async (intent: any) => {
      sentIntents.push(intent);
      return sendError;
    });

  const root = createRoot(host);
  act(() => {
    root.render(
      React.createElement(ChatCsvImport, { scope: SCOPE, canWrite: true, onSendIntent: sendIntent }),
    );
  });
  await settle(() => assert(host.querySelector("[data-chat-import]"), "chat import renders"));

  const fillCsv = async (value: string) => {
    const area = host.querySelector("#chat-csv-text") as HTMLTextAreaElement;
    assert(area, "the CSV textarea renders");
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(window.HTMLTextAreaElement.prototype, "value")!.set!;
      setter.call(area, value);
      area.dispatchEvent(new Event("input", { bubbles: true }));
    });
  };
  const click = async (el: Element | undefined | null) => {
    assert(el, "expected element to click");
    await act(async () => { (el as HTMLElement).click(); });
    await act(async () => { await Promise.resolve(); });
  };

  return { host, text, byText, fillCsv, click, posts, sentIntents, setSendError: (e: string | null) => { sendError = e; }, unmount: () => act(() => root.unmount()) };
}

async function main() {
  // ---- Happy path: preview → review → confirm → scoped-intent send ----
  {
    const m = await mount({});
    await m.fillCsv(CSV);
    await m.click(m.byText("button", "Preview plan"));
    await settle(() => assert(m.byText("button", "Confirm import"), "the Confirm control renders after preview"));
    // sanity: preview posted the exact csv_text, nothing else.
    equal(m.posts[0].path, "csv-preview", "the preview posted");
    equal(m.posts[0].body.csv_text, CSV, "preview sent the pasted bytes");
    // Confirm: the durable plan goes to csv-confirm; the message carries only the receipt.
    await m.click(m.byText("button", "Confirm import"));
    await settle(() => assert(m.sentIntents.length === 1, "the confirmed import handed off once"));
    const confirmBody = m.posts.find((p) => p.path === "csv-confirm")!.body;
    equal(confirmBody.csv_text, CSV, "confirm stores the exact CSV bytes");
    assert(typeof confirmBody.request_id === "string", "confirm carries the request id");
    assert(/^sha256:[0-9a-f]{64}$/.test(confirmBody.preview_token), "confirm carries the preview token");
    assert(/^sha256:[0-9a-f]{64}$/.test(confirmBody.decisions_digest), "confirm carries the decisions digest");
    // The scoped message carries ONLY request_id + confirm_token under the
    // import envelope — no csv_text, decisions or preview_token on the wire.
    const intent = m.sentIntents[0];
    equal(Object.keys(intent), ["cadence_csv_import"], "the intent is only the import envelope");
    equal(
      Object.keys(intent.cadence_csv_import).sort(),
      ["confirm_token", "request_id"],
      "the envelope carries only request_id + confirm_token",
    );
    equal(intent.cadence_csv_import.request_id, confirmBody.request_id, "the intent names the confirmed request");
    equal(intent.cadence_csv_import.confirm_token, "confirm-abc123", "the intent carries the minted token");
    assert(m.text().includes("import went to the assistant"), "the hand-off status shows");
    await m.unmount();
  }

  // ---- Cancel: no confirm posts, nothing imported ----
  {
    const m = await mount({});
    await m.fillCsv(CSV);
    await m.click(m.byText("button", "Preview plan"));
    await settle(() => assert(m.byText("button", "Confirm import"), "preview landed"));
    await m.click(m.byText("button", "Cancel"));
    await settle(() => assert(m.byText("button", "Preview plan"), "cancel returns to the paste step"));
    assert(!m.posts.some((p) => p.path === "csv-confirm"), "no confirm posted on cancel");
    assert(m.sentIntents.length === 0, "no intent sent on cancel");
    await m.unmount();
  }

  // ---- Refused confirm: the turn never starts, an error surfaces ----
  {
    const m = await mount({
      confirmResponse: () =>
        new Response(JSON.stringify({ error: "customer CSV preview token is out of bounds" }), {
          status: 409, headers: { "Content-Type": "application/json" },
        }),
    });
    await m.fillCsv(CSV);
    await m.click(m.byText("button", "Preview plan"));
    await settle(() => assert(m.byText("button", "Confirm import"), "preview landed"));
    await m.click(m.byText("button", "Confirm import"));
    await settle(() => assert(m.text().includes("bounds") || m.text().includes("refus"), "a refused confirm surfaces an error"));
    assert(m.sentIntents.length === 0, "a refused confirm starts no agent turn");
    await m.unmount();
  }

  // ---- Send-failure: confirm minted but the hand-off fails — honest error ----
  {
    const m = await mount({
      sendIntent: async () => "network unreachable",
    });
    await m.fillCsv(CSV);
    await m.click(m.byText("button", "Preview plan"));
    await settle(() => assert(m.byText("button", "Confirm import"), "preview landed"));
    await m.click(m.byText("button", "Confirm import"));
    await settle(() => assert(m.text().includes("did not start"), "a failed hand-off says so honestly"));
    assert(m.text().includes("Retry"), "the honest retry hint shows");
    await m.unmount();
  }

  // ---- Double-Confirm: at most one confirm post + one intent ----
  {
    const m = await mount({});
    await m.fillCsv(CSV);
    await m.click(m.byText("button", "Preview plan"));
    await settle(() => assert(m.byText("button", "Confirm import"), "preview landed"));
    const btn = m.byText("button", "Confirm import") as HTMLButtonElement;
    await m.click(btn);
    await m.click(btn); // second click — the pending guard disables it
    await settle(() => assert(m.sentIntents.length === 1, "at most one intent sent"));
    equal(m.posts.filter((p) => p.path === "csv-confirm").length, 1, "at most one confirm posted");
    await m.unmount();
  }

  console.log("chat csv import checks passed");
}

await main();
