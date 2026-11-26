/**
 * CAD-2841: Channels Wave 2 UI coverage — unread boundary resolution, room-scoped
 * attachment client & store routing, ChannelAttachments thumbnail/chip/lightbox
 * fallback rendering, safe inline formatting on Turn, and Composer paste /
 * empty-body attachment send.
 */
declare function require(name: string): any;
declare const process: { exit(code: number): never };
export {};

function check(cond: unknown, msg: string): void {
  if (!cond) throw new Error(`channelsWave2: ${msg}`);
}
function equal<T>(a: T, e: T, msg: string): void {
  if (JSON.stringify(a) !== JSON.stringify(e)) {
    throw new Error(`${msg}: expected ${JSON.stringify(e)}, got ${JSON.stringify(a)}`);
  }
}

const { Window } = require("happy-dom");
const win = new Window({ url: "https://board.example.test/threads" });
for (const name of [
  "window",
  "document",
  "Node",
  "Element",
  "HTMLElement",
  "HTMLImageElement",
  "HTMLButtonElement",
  "HTMLTextAreaElement",
  "HTMLFormElement",
  "Event",
  "MouseEvent",
  "KeyboardEvent",
  "ClipboardEvent",
  "navigator",
  "location",
  "localStorage",
  "sessionStorage",
  "ResizeObserver",
]) {
  if (win[name] !== undefined || name === "window") {
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  }
}
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const { renderToStaticMarkup } = require("react-dom/server") as typeof import("react-dom/server");
const { act } = React;

const loader = require("module");
const originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const { findFirstUnreadPostId } =
  require("../src/features/threads/channelsUnread") as typeof import("../src/features/threads/channelsUnread");
const Client =
  require("../src/features/threads/humanThreadsClient") as typeof import("../src/features/threads/humanThreadsClient");
const AttachMod =
  require("../src/features/app-shell/chat/attach") as typeof import("../src/features/app-shell/chat/attach");
const Store =
  require("../src/features/app-shell/chat/composerStore") as typeof import("../src/features/app-shell/chat/composerStore");
const ChannelAttachments = (
  require("../src/features/threads/ChannelAttachments") as typeof import("../src/features/threads/ChannelAttachments")
).default;
const { Turn } =
  require("../src/features/threads/ThreadChatParts") as typeof import("../src/features/threads/ThreadChatParts");
const Composer = (
  require("../src/features/app-shell/chat/Composer") as typeof import("../src/features/app-shell/chat/Composer")
).default;
const { LocaleTextProvider } =
  require("../src/lib/localeText") as typeof import("../src/lib/localeText");
const { LocaleProvider, translateUiText } =
  require("../src/lib/locale") as typeof import("../src/lib/locale");
loader.prototype.require = originalRequire;

const identity = (s: string) => s;

// ---- 1. findFirstUnreadPostId ----
{
  const me = "human-me";
  const posts = [
    { id: "p1", silent: false, sender: { id: "human-bob" } },
    { id: "p2", silent: false, sender: { id: me } },
    { id: "p3", silent: true, sender: { id: "human-bob" } },
    { id: "p4", silent: false, sender: { id: "human-bob" } },
    { id: "p5", silent: false, sender: { id: me } },
    { id: "p6", silent: false, sender: { id: "human-carla" } },
  ];
  equal(findFirstUnreadPostId(posts, 0, me), null, "0 unread returns null");
  equal(findFirstUnreadPostId(posts, 1, me), "p6", "1 unread skips nothing newer and lands on p6");
  equal(findFirstUnreadPostId(posts, 2, me), "p4", "2 unread skips own p5 and silent p3");
  equal(findFirstUnreadPostId(posts, 3, me), "p1", "3 unread lands on p1");
  equal(findFirstUnreadPostId(posts, 99, me), "p1", "excess unread falls back to oldest incoming post");
}

// ---- 2. humanThreadsClient upload, post attachment_ids, & room read validation ----
async function runAsyncChecks(): Promise<void> {
  const realFetch = globalThis.fetch;
  const calls: Array<{ url: string; method: string; headers: Headers; body: unknown }> = [];
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    const method = init?.method ?? "GET";
    const headers = new Headers(init?.headers);
    calls.push({ url, method, headers, body: init?.body });

    if (url.endsWith("/api/human-threads/rooms/grp_w2/attachments") && method === "POST") {
      return new Response(
        JSON.stringify({
          attachment: {
            id: "chf-00000000000000000000000000000001",
            name: "screen.png",
            mime: "image/png",
            size: 128,
            kind: "image",
            url: "/api/human-threads/rooms/grp_w2/attachments/chf-00000000000000000000000000000001",
          },
        }),
        { status: 201, headers: { "content-type": "application/json" } },
      );
    }
    if (url.endsWith("/api/human-threads/rooms/grp_w2/posts") && method === "POST") {
      const payload = JSON.parse(String(init?.body)) as { attachment_ids?: string[]; body: string };
      return new Response(
        JSON.stringify({
          created: true,
          post: {
            id: "hp_1",
            read_cursor: "csc1_abc123",
            sender: { id: "human-me", label: "Alice", kind: "human" },
            body: payload.body,
            created: 1000,
            silent: false,
            outcomes: [],
            attachments: (payload.attachment_ids ?? []).map((id) => ({
              id,
              name: "screen.png",
              mime: "image/png",
              size: 128,
              kind: "image",
              url: `/api/human-threads/rooms/grp_w2/attachments/${id}`,
            })),
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    if (url.includes("/api/human-threads/rooms/grp_bad") && method === "GET") {
      return new Response(
        JSON.stringify({
          room: "grp_bad",
          scope_id: "csrid_123456",
          members: [{ kind: "human", id: "human-me", label: "Alice" }],
          posts: [
            {
              id: "hp_bad",
              read_cursor: "csc1_abc123",
              sender: { id: "human-me", label: "Alice", kind: "human" },
              body: "bad",
              created: 1000,
              silent: false,
              outcomes: [],
              attachments: [
                {
                  id: "chf-111",
                  name: "phish.png",
                  mime: "image/png",
                  size: 10,
                  kind: "image",
                  url: "https://evil.example/phish.png",
                },
              ],
            },
          ],
          events: [],
          before: "csc1_abc123",
          more_before: false,
          can_ask: false,
          can_manage: true,
          agent_chain_count: 0,
          agent_chain_limit: 2,
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    throw new Error(`Unexpected fetch: ${method} ${url}`);
  }) as typeof fetch;

  try {
    const file = new File([new Uint8Array([137, 80, 78, 71])], "screen.png", { type: "image/png" });
    const uploaded = await Client.uploadHumanRoomAttachment("grp_w2", file);
    equal(uploaded.id, "chf-00000000000000000000000000000001", "upload returns server attachment");
    check(calls[0]!.body instanceof FormData, "upload sends FormData");
    equal(calls[0]!.headers.get("X-Cadence-Board"), "1", "upload includes X-Cadence-Board header");
    equal(calls[0]!.headers.get("Content-Type"), null, "multipart upload omits manual Content-Type");

    const posted = await Client.postHumanRoom("grp_w2", "cid-1", "", false, [uploaded.id]);
    equal(posted.post.attachments?.[0]?.id, uploaded.id, "post returns attached item");
    const sentJson = JSON.parse(String(calls[1]!.body)) as { attachment_ids?: string[]; body: string };
    equal(sentJson.attachment_ids, [uploaded.id], "post sends attachment_ids");

    let rejectedBadUrl = false;
    try {
      await Client.readHumanRoom("grp_bad");
    } catch {
      rejectedBadUrl = true;
    }
    check(rejectedBadUrl, "readHumanRoom rejects non-room-scoped external attachment URL");

    // ---- 3. sameAttachDestination & composerStore humanRoom upload routing ----
    check(
      AttachMod.sameAttachDestination({ humanRoom: "grp_w2" }, { humanRoom: "grp_w2" }),
      "same humanRoom matches",
    );
    check(
      !AttachMod.sameAttachDestination({ humanRoom: "grp_w1" }, { humanRoom: "grp_w2" }),
      "different humanRoom is isolated",
    );
    check(
      !AttachMod.sameAttachDestination({ humanRoom: "grp_w2" }, { conversation: "tc_1" }),
      "humanRoom never crosses to Home conversation",
    );

    const storeKey = "threads-human:grp_w2";
    const container = document.createElement("div");
    document.body.appendChild(container);
    const root = createRoot(container);
    const sentPayloads: Array<{ body: string; attachmentIds: string[] }> = [];

    await act(async () => {
      root.render(
        React.createElement(
          LocaleProvider,
          null,
          React.createElement(Composer, {
            density: "full",
            block: null,
            storeKey,
            attach: {
              enabled: true,
              reason: "",
              destination: { humanRoom: "grp_w2" },
            },
            allowEmptyWithAttachments: true,
            onSend: async ({ body, attachments }) => {
              sentPayloads.push({
                body,
                attachmentIds: (attachments ?? []).map((a) => a.id),
              });
            },
          }),
        ),
      );
    });

    // Simulate clipboard screenshot paste without extension -> normalizes to screenshot-*.png & uploads
    const callsBeforePaste = calls.length;
    const pasteFile = new File([new Uint8Array([1, 2, 3, 4])], "image.png", { type: "image/png" });
    const unnamedPasteFile = new File([new Uint8Array([1, 2, 3, 4])], "", { type: "image/png" });
    void pasteFile;
    const textarea = container.querySelector("textarea")!;
    await act(async () => {
      const pasteEvent = new Event("paste", { bubbles: true, cancelable: true }) as Event & {
        clipboardData: { files: File[]; items: never[] };
      };
      Object.defineProperty(pasteEvent, "clipboardData", {
        value: { files: [unnamedPasteFile], items: [] },
      });
      textarea.dispatchEvent(pasteEvent);
    });
    // Drain microtasks until upload completes cleanly via Store state
    for (let i = 0; i < 20 && Store.attachItems(storeKey).some((item) => item.status === "uploading"); i += 1) {
      await act(async () => {
        await Promise.resolve();
      });
    }
    check(calls.length > callsBeforePaste, "pasted screenshot triggers uploadHumanRoomAttachment");
    const pastedForm = calls[callsBeforePaste]!.body as FormData;
    const uploadedFile = pastedForm.get("file") as File;
    check(
      uploadedFile.name.startsWith("screenshot-") && uploadedFile.name.endsWith(".png"),
      "unnamed clipboard image normalized to screenshot-*.png before upload",
    );
    const queued = Store.attachItems(storeKey);
    equal(queued.length, 1, "one pasted attachment queued");
    equal(queued[0]!.status, "ready", "pasted attachment finishes uploading");

    // Empty body with ready attachment is sendable!
    const sendButton = container.querySelector<HTMLButtonElement>('button[type="submit"]')!;
    equal(sendButton.disabled, false, "Send button enabled for attachment-only message");
    await act(async () => {
      container.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    });
    await act(async () => {
      await Promise.resolve();
    });
    equal(sentPayloads.length, 1, "attachment-only message submitted");
    equal(sentPayloads[0]!.body, "", "body stayed empty");
    equal(sentPayloads[0]!.attachmentIds, ["chf-00000000000000000000000000000001"], "attachment id carried to onSend");
    equal(Store.attachItems(storeKey).length, 0, "sent attachment cleared from queue");

    act(() => root.unmount());
    container.remove();
  } finally {
    globalThis.fetch = realFetch;
  }
}

// ---- 4. ChannelAttachments & AttachmentLightbox & error fallback ----
async function runAttachmentComponentChecks(): Promise<void> {
  const png: import("../src/features/threads/humanThreadsClient").HumanPostAttachment = {
    id: "chf-00000000000000000000000000000010",
    name: "diag.svg",
    mime: "image/svg+xml",
    size: 512,
    kind: "image",
    url: "/api/human-threads/rooms/grp_1/attachments/chf-00000000000000000000000000000010",
  };
  const csv: import("../src/features/threads/humanThreadsClient").HumanPostAttachment = {
    id: "chf-00000000000000000000000000000011",
    name: "budget.csv",
    mime: "text/csv",
    size: 2048,
    kind: "text",
    url: "/api/human-threads/rooms/grp_1/attachments/chf-00000000000000000000000000000011",
  };
  const pdf: import("../src/features/threads/humanThreadsClient").HumanPostAttachment = {
    id: "chf-00000000000000000000000000000012",
    name: "invoice.pdf",
    mime: "application/pdf",
    size: 4096,
    kind: "pdf",
    url: "/api/human-threads/rooms/grp_1/attachments/chf-00000000000000000000000000000012",
  };

  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  await act(async () => {
    root.render(
      React.createElement(
        LocaleTextProvider,
        { value: identity },
        React.createElement(ChannelAttachments, { attachments: [png, csv, pdf] }),
      ),
    );
  });

  const thumbBtn = container.querySelector<HTMLButtonElement>("button.sentthumb");
  check(thumbBtn !== null, "raster/svg renders rounded thumbnail button");
  const thumbImg = container.querySelector<HTMLImageElement>("button.sentthumb img");
  equal(thumbImg?.getAttribute("src"), png.url, "thumbnail points to room-scoped url");

  const csvLink = container.querySelector<HTMLAnchorElement>('a.threads-file-chip[href]');
  equal(csvLink?.getAttribute("href"), csv.url, "CSV renders download chip linking to room attachment");
  equal(csvLink?.getAttribute("download"), "budget.csv", "CSV chip includes download attribute");

  const pdfBtn = container.querySelector<HTMLButtonElement>('button.threads-file-chip[data-kind="pdf"]');
  check(pdfBtn !== null, "PDF renders button chip (blob tab open)");

  // Clicking thumbnail opens lightbox using room-scoped URL on image & Download link
  await act(async () => {
    thumbBtn!.click();
  });
  const dialog = document.body.querySelector<HTMLElement>('.sentlight[role="dialog"]');
  check(dialog !== null, "clicking thumbnail opens lightbox dialog");
  equal(dialog!.querySelector("img")?.getAttribute("src"), png.url, "lightbox image uses room url");
  equal(dialog!.querySelector("a[download]")?.getAttribute("href"), png.url, "lightbox Download uses room url");
  await act(async () => {
    dialog!.querySelector<HTMLButtonElement>(".sentlight-bar button")?.click();
  });
  equal(document.body.querySelector('.sentlight[role="dialog"]'), null, "Close dismisses lightbox");

  // Failed image error falls back to file chip
  await act(async () => {
    container.querySelector<HTMLImageElement>("button.sentthumb img")!.dispatchEvent(new Event("error"));
  });
  equal(container.querySelector("button.sentthumb"), null, "broken thumbnail removed after error");
  check(
    container.querySelector('a.threads-file-chip[download="diag.svg"]') !== null,
    "errored media falls back to download chip",
  );

  act(() => root.unmount());
  container.remove();
}

// ---- 5. Turn safe inline formatting & empty-body attachment post ----
{
  const html = renderToStaticMarkup(
    React.createElement(
      LocaleTextProvider,
      { value: identity },
      React.createElement(Turn, {
        who: "Bob",
        inlineFormat: true,
        children:
          "**Ship** `v2` *now* https://example.test/doc ![pixel](https://evil.test/x.png) <script>alert(1)</script>\n```\nconst a = 1;\n```",
      }),
    ),
  );
  check(html.includes("<strong>Ship</strong>"), "strong renders");
  check(html.includes('<code class="op-code">v2</code>'), "inline code renders");
  check(html.includes("<em>now</em>"), "italic renders");
  check(html.includes('<a class="lnk" href="https://example.test/doc" target="_blank" rel="noopener noreferrer">'), "safe external link renders with noopener");
  check(html.includes('<pre class="op-pre num">const a = 1;</pre>'), "fenced code block renders");
  check(!html.includes("<img"), "markdown image never renders remote <img> pixel");
  check(!html.includes("<script>"), "raw HTML script tag never becomes an element");
  check(html.includes("&lt;script&gt;alert(1)&lt;/script&gt;"), "raw HTML stays escaped text");

  const attOnly = renderToStaticMarkup(
    React.createElement(
      LocaleTextProvider,
      { value: identity },
      React.createElement(Turn, {
        who: "You",
        me: true,
        inlineFormat: true,
        attachments: React.createElement("div", { "data-att-slot": "1" }),
        children: "",
      }),
    ),
  );
  check(!attOnly.includes("chat-bubble"), "empty-body attachment post omits empty chat bubble");
  check(attOnly.includes('data-att-slot="1"'), "empty-body attachment post still renders attachments");

  // ZH translation check for "New messages" divider label
  equal(translateUiText("New messages", "zh-TW"), "新訊息", "New messages translates in zh-TW");
}

runAsyncChecks()
  .then(runAttachmentComponentChecks)
  .then(() => {
    console.log("channelsWave2.test.tsx: all assertions passed");
  })
  .catch((err) => {
    console.error(err);
    process.exit(1);
  });
