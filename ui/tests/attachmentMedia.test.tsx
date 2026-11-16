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
ok(!markup.includes(`src="${attachmentHref(IMAGE.id, CONV)}"`), "the thumbnail never exposes an unpinned route");
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

// ---- image previews remain lazy and scoped to the verified session ----
async function imagePinChecks(): Promise<void> {
  const { allowPrivateReads, pausePrivateReads, resetPrivateSession } = require("../src/lib/privateSession") as typeof import("../src/lib/privateSession");
  const sessionId = "bsi_11111111-1111-4111-8111-111111111111";
  const realFetch = globalThis.fetch;
  const realObserver = globalThis.IntersectionObserver;
  const realRevoke = URL.revokeObjectURL;
  const revoked: string[] = [];
  URL.revokeObjectURL = (url) => { revoked.push(url); realRevoke.call(URL, url); };
  let notify: IntersectionObserverCallback | undefined;
  let requested: string | null = null;
  let requests = 0;
  class Observer {
    constructor(callback: IntersectionObserverCallback) { notify = callback; }
    observe() {}
    disconnect() {}
  }
  globalThis.IntersectionObserver = Observer as unknown as typeof IntersectionObserver;
  globalThis.fetch = (async (_input: RequestInfo | URL, init?: RequestInit) => {
    requests++;
    requested = new Headers(init?.headers).get("X-Cadence-Expected-Session");
    return new Response(new Uint8Array([137, 80, 78, 71]), {
      headers: { "Content-Type": "image/png", "X-Cadence-Verified-Session": sessionId },
    });
  }) as typeof fetch;
  const host = document.createElement("div");
  document.body.append(host);
  const imageRoot = createRoot(host);
  try {
    resetPrivateSession();
    allowPrivateReads(sessionId);
    await React.act(async () => {
      imageRoot.render(React.createElement(AttachmentMedia, { files: [IMAGE], conversation: CONV, hosted: true }));
      for (let i = 0; i < 10; i++) await Promise.resolve();
    });
    equal(requests, 0, "an offscreen image does not fetch");
    ok(notify, "an image observes visibility before fetching");
    await React.act(async () => {
      notify!([{ isIntersecting: true } as IntersectionObserverEntry], {} as IntersectionObserver);
      for (let i = 0; i < 30; i++) await Promise.resolve();
    });
    equal(requested, sessionId, "an image read pins the verified hosted session");
    const thumb = host.querySelector<HTMLButtonElement>(".sentthumb");
    const image = thumb?.querySelector("img");
    ok(image?.getAttribute("src")?.startsWith("blob:"), "matching proof paints an object URL");
    const verifiedSource = image!.getAttribute("src")!;
    await React.act(async () => { thumb!.click(); });
    const download = document.querySelector<HTMLAnchorElement>(".sentlight a[download]");
    equal(download?.getAttribute("href"), image?.getAttribute("src"), "the lightbox download reuses the verified image");
    equal(document.querySelector(".sentlight")?.getAttribute("role"), "dialog", "the lightbox is a dialog");
    await React.act(async () => { document.dispatchEvent(new win.KeyboardEvent("keydown", { key: "Escape" })); });
    ok(!document.querySelector(".sentlight"), "Escape closes the lightbox");
    await React.act(async () => { thumb!.click(); });
    await React.act(async () => { pausePrivateReads(); });
    ok(!host.querySelector("img[src^='blob:']") && !document.querySelector(".sentlight a[href^='blob:']"), "pausing private reads removes image access");
    ok(revoked.includes(verifiedSource), "pausing private reads revokes the image URL");
    await React.act(async () => { image!.dispatchEvent(new win.Event("error")); });
    ok(host.querySelector(".sentthumb"), "a pause-induced image error does not discard the preview");
  } finally {
    await React.act(async () => { imageRoot.unmount(); });
    host.remove();
    globalThis.fetch = realFetch;
    globalThis.IntersectionObserver = realObserver;
    URL.revokeObjectURL = realRevoke;
    resetPrivateSession();
  }
}

async function imageRefusalChecks(): Promise<void> {
  const { allowPrivateReads, resetPrivateSession } = require("../src/lib/privateSession") as typeof import("../src/lib/privateSession");
  const sessionId = "bsi_11111111-1111-4111-8111-111111111111";
  const otherSession = "bsi_22222222-2222-4222-8222-222222222222";
  const realFetch = globalThis.fetch;
  try {
    for (const proof of [null, otherSession]) {
      let requestSession: string | null = null;
      globalThis.fetch = (async (_input: RequestInfo | URL, init?: RequestInit) => {
        requestSession = new Headers(init?.headers).get("X-Cadence-Expected-Session");
        return new Response("synthetic image", {
          headers: proof ? { "X-Cadence-Verified-Session": proof } : {},
        });
      }) as typeof fetch;
      const host = document.createElement("div");
      document.body.append(host);
      const imageRoot = createRoot(host);
      try {
        resetPrivateSession();
        allowPrivateReads(sessionId);
        await React.act(async () => {
          imageRoot.render(React.createElement(AttachmentMedia, { files: [IMAGE], conversation: CONV, hosted: true }));
          for (let i = 0; i < 30; i++) await Promise.resolve();
        });
        equal(requestSession, sessionId, "the image request uses the admitted identity");
        ok(!host.querySelector("img[src^='blob:']") && !host.querySelector(".sentthumb"), "a missing or mismatched proof leaves no image");
      } finally {
        await React.act(async () => { imageRoot.unmount(); });
        host.remove();
      }
    }

    const secondImage = { ...IMAGE, id: `chf-${"d".repeat(32)}`, name: "second.png" };
    globalThis.fetch = (async () => new Response("synthetic image", {
      headers: { "X-Cadence-Verified-Session": sessionId },
    })) as typeof fetch;
    const galleryHost = document.createElement("div");
    document.body.append(galleryHost);
    const galleryRoot = createRoot(galleryHost);
    try {
      resetPrivateSession();
      allowPrivateReads(sessionId);
      await React.act(async () => {
        galleryRoot.render(React.createElement(AttachmentMedia, { files: [IMAGE, secondImage], conversation: CONV, hosted: true }));
        for (let i = 0; i < 30; i++) await Promise.resolve();
      });
      await React.act(async () => { galleryHost.querySelector<HTMLButtonElement>(".sentthumb")!.click(); });
      await React.act(async () => { document.dispatchEvent(new win.KeyboardEvent("keydown", { key: "ArrowRight" })); });
      equal(document.querySelector(".sentlight img")?.getAttribute("alt"), "second.png", "arrow navigation opens the next verified image");
      ok(document.querySelector(".sentlight a[href^='blob:']"), "gallery download stays on a verified blob");
      await React.act(async () => { galleryHost.querySelector("img")!.dispatchEvent(new win.Event("error")); });
      equal(galleryHost.querySelectorAll(".sentthumb").length, 1, "a genuine image error removes only the failed preview");
    } finally {
      await React.act(async () => { galleryRoot.unmount(); });
      galleryHost.remove();
    }

    let deliverNext!: (response: Response) => void;
    let reads = 0;
    globalThis.fetch = (async () => {
      if (++reads === 1)
        return new Response("first image", { headers: { "X-Cadence-Verified-Session": sessionId } });
      return new Promise<Response>((resolve) => { deliverNext = resolve; });
    }) as typeof fetch;
    const switchedHost = document.createElement("div");
    document.body.append(switchedHost);
    const switchedRoot = createRoot(switchedHost);
    try {
      resetPrivateSession();
      allowPrivateReads(sessionId);
      await React.act(async () => {
        switchedRoot.render(React.createElement(AttachmentMedia, { files: [IMAGE], conversation: CONV, hosted: true }));
        for (let i = 0; i < 30; i++) await Promise.resolve();
      });
      const originalSource = switchedHost.querySelector("img")?.getAttribute("src");
      ok(originalSource?.startsWith("blob:"), "the first conversation has an image");
      await React.act(async () => { switchedHost.querySelector<HTMLButtonElement>(".sentthumb")!.click(); });
      await React.act(async () => {
        switchedRoot.render(React.createElement(AttachmentMedia, { files: [IMAGE], conversation: "tc_next", hosted: true }));
        for (let i = 0; i < 20; i++) await Promise.resolve();
      });
      ok(switchedHost.querySelector("img")?.getAttribute("src") !== originalSource, "a new conversation cannot inherit the old image");
      ok(!document.querySelector(".sentlight"), "changing conversations closes the old lightbox");
      ok(typeof deliverNext === "function", "the new conversation has its own pending read");
      await React.act(async () => {
        deliverNext(new Response("second image", { headers: { "X-Cadence-Verified-Session": sessionId } }));
        for (let i = 0; i < 30; i++) await Promise.resolve();
      });
      ok(switchedHost.querySelector("img")?.getAttribute("src")?.startsWith("blob:"), "the new conversation can load its own image");
    } finally {
      await React.act(async () => { switchedRoot.unmount(); });
      switchedHost.remove();
    }

    let finishBody!: (body: Blob) => void;
    const response = new Response(null, { headers: { "X-Cadence-Verified-Session": sessionId } });
    response.blob = () => new Promise<Blob>((resolve) => { finishBody = resolve; });
    globalThis.fetch = (async () => response) as typeof fetch;
    const host = document.createElement("div");
    document.body.append(host);
    const imageRoot = createRoot(host);
    try {
      resetPrivateSession();
      allowPrivateReads(sessionId);
      await React.act(async () => {
        imageRoot.render(React.createElement(AttachmentMedia, { files: [IMAGE], conversation: CONV, hosted: true }));
        for (let i = 0; i < 30; i++) await Promise.resolve();
      });
      ok(typeof finishBody === "function", "the image body read has started");
      await React.act(async () => {
        resetPrivateSession();
        allowPrivateReads(otherSession);
        finishBody(new Blob(["old image"]));
        for (let i = 0; i < 30; i++) await Promise.resolve();
      });
      ok(!host.querySelector("img[src^='blob:']"), "an old image body cannot appear after identity reset");
    } finally {
      await React.act(async () => { imageRoot.unmount(); });
      host.remove();
    }
  } finally {
    globalThis.fetch = realFetch;
    resetPrivateSession();
  }
}

declare const process: { exitCode: number };
// ---- a PDF opens through a fetched object URL, never the bare route ----
async function pdfCheck(): Promise<void> {
  const { openAttachmentInTab } = AttachmentMediaModule;
  const { allowPrivateReads, resetPrivateSession } = require("../src/lib/privateSession") as typeof import("../src/lib/privateSession");
  const sessionId = "bsi_11111111-1111-4111-8111-111111111111";
  const otherSession = "bsi_22222222-2222-4222-8222-222222222222";
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
  let requestedSession: string | null = null;
  allowPrivateReads(sessionId);
  globalThis.fetch = (async (_input: RequestInfo | URL, init?: RequestInit) => {
    requestedSession = new Headers(init?.headers).get("X-Cadence-Expected-Session");
    return new Response(new Uint8Array([0x25, 0x50, 0x44, 0x46]), {
      status: 200,
      headers: { "content-type": "application/pdf", "X-Cadence-Verified-Session": sessionId },
    });
  }) as typeof fetch;
  await React.act(async () => {
    openAttachmentInTab(PDF.id, CONV);
    for (let i = 0; i < 20; i++) await Promise.resolve();
  });
  ok(replaced.startsWith("blob:"), "the PDF opens through a fetched object URL");
  equal(requestedSession, sessionId, "the PDF read pins the verified hosted session");
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

  for (const proof of [null, otherSession]) {
    replaced = "";
    closed = false;
    globalThis.fetch = (async () => new Response("synthetic", {
      status: 200,
      headers: proof ? { "X-Cadence-Verified-Session": proof } : {},
    })) as typeof fetch;
    await React.act(async () => {
      openAttachmentInTab(PDF.id, CONV);
      for (let i = 0; i < 20; i++) await Promise.resolve();
    });
    ok(closed && replaced === "", "a missing or mismatched server session proof opens no PDF");
  }

  replaced = "";
  closed = false;
  let unverifiedRequest = false;
  allowPrivateReads();
  globalThis.fetch = (async () => { unverifiedRequest = true; return new Response("synthetic"); }) as typeof fetch;
  await React.act(async () => {
    openAttachmentInTab(PDF.id, CONV);
    for (let i = 0; i < 20; i++) await Promise.resolve();
  });
  ok(closed && replaced === "" && !unverifiedRequest, "a PDF cannot open without a verified hosted session");
  allowPrivateReads(sessionId);

  // A response from the previous account must not open after an identity
  // reset, even if a transport ignores its abort signal.
  replaced = "";
  closed = false;
  let deliver!: (response: Response) => void;
  const request: { signal?: AbortSignal } = {};
  globalThis.fetch = ((_url: RequestInfo | URL, init?: RequestInit) => {
    request.signal = init?.signal ?? undefined;
    return new Promise<Response>((resolve) => { deliver = resolve; });
  }) as typeof fetch;
  await React.act(async () => {
    openAttachmentInTab(PDF.id, CONV);
    resetPrivateSession();
    allowPrivateReads(otherSession);
    deliver(new Response("synthetic account A attachment", { status: 200, headers: { "X-Cadence-Verified-Session": sessionId } }));
    for (let i = 0; i < 20; i++) await Promise.resolve();
  });
  ok(request.signal?.aborted, "the retired account's request is aborted");
  ok(closed, "the previous account's blank tab closes after a delayed response");
  ok(replaced === "", "the previous account's bytes never open after the switch");

  // A reset after headers but during an uncooperative body read is fenced too.
  replaced = "";
  closed = false;
  let finishBody!: (body: Blob) => void;
  const bodyResponse = new Response(null, { status: 200, headers: { "X-Cadence-Verified-Session": sessionId } });
  bodyResponse.blob = () => new Promise<Blob>((resolve) => { finishBody = resolve; });
  globalThis.fetch = (async () => bodyResponse) as typeof fetch;
  allowPrivateReads(sessionId);
  await React.act(async () => {
    openAttachmentInTab(PDF.id, CONV);
    for (let i = 0; i < 10; i++) await Promise.resolve();
    ok(typeof finishBody === "function", "the attachment body read has started");
    resetPrivateSession();
    allowPrivateReads(otherSession);
    finishBody(new Blob(["synthetic account A attachment"]));
    for (let i = 0; i < 20; i++) await Promise.resolve();
  });
  ok(closed, "a switched session closes the tab during an in-flight body read");
  ok(replaced === "", "an old body cannot open after an account switch");

  globalThis.fetch = realFetch;
  win.open = realOpen;
  win.setTimeout = realSetTimeout;
  console.log("attachment media passed");
}
void imagePinChecks().then(imageRefusalChecks).then(pdfCheck).catch((error: unknown) => {
  console.error(error);
  process.exitCode = 1;
});
