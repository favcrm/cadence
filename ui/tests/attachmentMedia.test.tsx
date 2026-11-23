/**
 * CAD-2219: an operator's image or clip in their own bubble. The component is
 * rendered both to static markup (the part a server pass must produce) and
 * into happy-dom so the thumbnail, lightbox, Escape and the failed-load
 * fallback are real interactions, not asserted source.
 */
declare function require(name: string): any;
export {};

function ok(cond: unknown, why: string): void {
  if (!cond) throw new Error(`failed: ${why}`);
}
function equal<T>(a: T, e: T, why: string): void {
  if (JSON.stringify(a) !== JSON.stringify(e))
    throw new Error(`${why}: expected ${JSON.stringify(e)}, got ${JSON.stringify(a)}`);
}

const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const name of [
  "window",
  "document",
  "Node",
  "Element",
  "HTMLElement",
  "HTMLImageElement",
  "HTMLButtonElement",
  "navigator",
  "Event",
  "MouseEvent",
  "KeyboardEvent",
  "location",
]) {
  Object.defineProperty(globalThis, name, {
    value: name === "window" ? win : win[name],
    configurable: true,
    writable: true,
  });
}
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const { renderToStaticMarkup } = require("react-dom/server") as typeof import("react-dom/server");
const AttachmentMediaModule = require("../src/features/home/AttachmentMedia") as typeof import("../src/features/home/AttachmentMedia");
const AttachmentMedia = AttachmentMediaModule.default;
const { attachmentHref, attachmentPreviewKind } = AttachmentMediaModule;

const CONV = "tc_00000000-0000-4000-8000-000000000001";
const IMAGE = { id: `chf-${"a".repeat(32)}`, name: "shot.png", size: 1024, mime: "image/png" };
const CLIP = { id: `chf-${"b".repeat(32)}`, name: "clip.mp4", size: 2048, mime: "video/mp4" };
const PDF = { id: `chf-${"c".repeat(32)}`, name: "brief.pdf", size: 4096, mime: "application/pdf" };


// ---- static markup: what a server pass must produce ----
const markup = renderToStaticMarkup(
  React.createElement(AttachmentMedia, { files: [IMAGE, CLIP, PDF], conversation: CONV, hosted: true }),
);
ok(markup.includes("data-entry-media"), "the media row renders");
ok(markup.includes(`src="${attachmentHref(IMAGE.id, CONV)}"`), "the thumbnail names the scoped route");
ok(markup.includes('alt="shot.png"'), "the thumbnail carries the file name");
ok(markup.includes('loading="lazy"'), "the thumbnail loads lazily");
ok(markup.includes("<video"), "a clip renders a player");
ok(markup.includes(`src="${attachmentHref(CLIP.id, CONV)}"`), "the player names the scoped route");
ok(!markup.includes("brief.pdf"), "a PDF keeps only its chip, never a picture");
equal(
  renderToStaticMarkup(React.createElement(AttachmentMedia, { files: [PDF], conversation: CONV, hosted: true })),
  "",
  "no previewable file means no media row",
);
equal(
  renderToStaticMarkup(React.createElement(AttachmentMedia, { files: [IMAGE], conversation: null, hosted: true })),
  "",
  "outside a host there is no conversation to read",
);
equal(
  renderToStaticMarkup(React.createElement(AttachmentMedia, { files: [IMAGE], conversation: CONV, hosted: false })),
  "",
  "a local board keeps the chip and requests nothing",
);
equal(attachmentPreviewKind("image/png"), "image", "png previews");
equal(attachmentPreviewKind("image/jpeg"), "image", "jpeg previews");
equal(attachmentPreviewKind("image/webp"), "image", "webp previews");
equal(attachmentPreviewKind("image/gif"), null, "gif is not a served raster");
equal(attachmentPreviewKind("video/webm"), "video", "webm plays");
equal(attachmentPreviewKind("video/quicktime"), null, "quicktime is not a stored container");
equal(attachmentPreviewKind("application/pdf"), null, "pdf is not media");
equal(
  attachmentHref("chf-abc", "tc x/y"),
  "/api/chat/attachments/chf-abc?conversation=tc%20x%2Fy",
  "the href encodes the conversation",
);

// ---- mounted: the thumbnail opens a lightbox, Escape closes it ----
const container = document.createElement("div");
document.body.appendChild(container);
const root = createRoot(container);
React.act(() => {
  root.render(React.createElement(AttachmentMedia, { files: [IMAGE], conversation: CONV, hosted: true }));
});
const thumb = container.querySelector(".sentthumb");
ok(thumb, "the thumbnail is a button");
React.act(() => {
  thumb!.dispatchEvent(new win.MouseEvent("click", { bubbles: true }));
});
const dialog = document.querySelector(".sentlight");
ok(dialog, "clicking the thumbnail opens the lightbox");
equal(dialog!.getAttribute("role"), "dialog", "the lightbox is a dialog");
ok(document.querySelector(".sentlight-inner img"), "the lightbox shows the picture");
ok(document.querySelector(".sentlight-bar a[download]"), "the lightbox offers a download");
React.act(() => {
  document.dispatchEvent(new win.KeyboardEvent("keydown", { key: "Escape" }));
});
ok(!document.querySelector(".sentlight"), "Escape closes the lightbox");

// ---- a failed load leaves the chip alone ----
const picture = container.querySelector(".sentthumb img");
ok(picture, "the thumbnail paints an image");
React.act(() => {
  picture!.dispatchEvent(new win.Event("error"));
});
ok(!container.querySelector(".sentthumb"), "a failed load removes the picture");
React.act(() => {
  root.unmount();
});

// ---- a refused clip falls back the same way (no bare player) ----
const clips = document.createElement("div");
document.body.appendChild(clips);
const clipRoot = createRoot(clips);
React.act(() => {
  clipRoot.render(React.createElement(AttachmentMedia, { files: [CLIP], conversation: CONV, hosted: true }));
});
const player = clips.querySelector("video");
ok(player, "a clip renders a player");
React.act(() => {
  player!.dispatchEvent(new win.Event("error"));
});
ok(!clips.querySelector(".sentclip"), "a refused clip leaves no bare player");
React.act(() => {
  clipRoot.unmount();
});

// ---- a PDF opens through a fetched object URL, never the bare route ----
declare const process: { exitCode: number };
async function pdfCheck(): Promise<void> {
  const { openAttachmentInTab } = AttachmentMediaModule;
  const realFetch = globalThis.fetch;
  const realOpen = win.open;
  const realSetTimeout = win.setTimeout;
  let replaced = "";
  let closed = false;
  win.open = (() => ({
    location: { replace: (url: string) => { replaced = url; } },
    close: () => { closed = true; },
  })) as unknown as typeof win.open;
  win.setTimeout = (() => 0) as unknown as typeof win.setTimeout;
  globalThis.fetch = (async () =>
    new Response(new Uint8Array([0x25, 0x50, 0x44, 0x46]), {
      status: 200,
      headers: { "content-type": "application/pdf" },
    })) as typeof fetch;
  await React.act(async () => {
    openAttachmentInTab(PDF.id, CONV);
    for (let i = 0; i < 20; i++) await Promise.resolve();
  });
  ok(replaced.startsWith("blob:"), "the PDF opens through a fetched object URL");
  ok(!closed, "a successful read keeps the tab");

  replaced = "";
  closed = false;
  globalThis.fetch = (async () => new Response(null, { status: 404 })) as typeof fetch;
  await React.act(async () => {
    openAttachmentInTab(PDF.id, CONV);
    for (let i = 0; i < 20; i++) await Promise.resolve();
  });
  ok(closed, "a refused read closes the blank tab");
  ok(replaced === "", "a refused read opens nothing");

  globalThis.fetch = realFetch;
  win.open = realOpen;
  win.setTimeout = realSetTimeout;
  console.log("attachment media passed");
}
void pdfCheck().catch((error: unknown) => {
  console.error(error);
  process.exitCode = 1;
});
