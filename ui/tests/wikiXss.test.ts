import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import Md from "../src/ui/Md";
import { previewKind } from "../src/features/wiki/preview";
import type * as ReactModule from "react";
import type * as ReactDomClient from "react-dom/client";
import type * as WikiModule from "../src/features/wiki/Wiki";
import type * as LocaleModule from "../src/lib/locale";
import type * as SessionModule from "../src/lib/sessionKey";
import type * as PrivateSessionModule from "../src/lib/privateSession";
import type * as WikiApiModule from "../src/features/wiki/api";
import type { Window as TestWindow } from "happy-dom";

/**
 * CAD-581's XSS probes on the wiki's markdown path. The page view renders
 * through the board's Md component (react-markdown, no raw-HTML plugin);
 * these render that component to static markup and assert that the three
 * classic injections never survive: a <script> tag, an onerror handler on
 * raw HTML, and a javascript: link.
 */

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

const render = (text: string) => renderToStaticMarkup(createElement(Md, { text }));

// Probe 1 — a script tag in a page's markdown is text, never an element.
const script = render("<script>alert(1)</script>");
equal(/<script/i.test(script), false, "no script element is produced");
equal(script.includes("&lt;script&gt;alert(1)&lt;/script&gt;"), true, "the tag renders escaped as text");

// Probe 2 — raw HTML with an event handler is text too.
const onerror = render('<img src=x onerror="alert(1)">');
equal(/<img/i.test(onerror), false, "no img element is produced");
equal(onerror.includes("&lt;img"), true, "the tag renders escaped as text");
const svg = render('<svg onload="alert(1)"></svg>');
equal(/<svg/i.test(svg), false, "no svg element is produced");

// Probe 3 — a javascript: link loses its target.
const js = render("[click](javascript:alert(1))");
equal(/javascript:/i.test(js), false, "the javascript: url is stripped");
equal(/<a /.test(js), false, "no anchor renders for a blanked target (CAD-1070)");
equal(js.includes(">click</p>"), true, "the link's text still shows");
const mixed = render("[x](JaVaScRiPt:alert(1))");
equal(/javascript:/i.test(mixed), false, "case does not smuggle the scheme through");
const img = render("![x](javascript:alert(1))");
equal(/javascript:/i.test(img), false, "an image src cannot be a javascript: url");
const cdnImage = render("![preview](https://scontent.fbcdn.net/track.jpg?marker=agent-text)");
equal(/<img/i.test(cdnImage), false, "agent Markdown cannot auto-load a newly CSP-allowed CDN image");
equal(cdnImage.includes("preview"), true, "blocked Markdown image keeps its alt text");
const localImage = render("![local](/assets/local.png)");
equal(localImage.includes('src="/assets/local.png"'), true, "same-origin Markdown images keep rendering");
equal(localImage.includes('href="/assets/local.png"'), true, "a same-origin Markdown image opens full size");
const data = render("[x](data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==)");
equal(/data:text\/html/i.test(data), false, "a data:text/html link is stripped too");

// Probe 4 — defence in depth: an uploaded SVG or HTML never previews
// inline, whatever MIME the server sniffed (the preview pane downloads).
equal(previewKind("evil.svg", "image/svg+xml"), "download", "an uploaded svg downloads");
equal(previewKind("evil.html", "text/html"), "download", "an uploaded html downloads");
equal(previewKind("evil.xhtml", null), "download", "an uploaded xhtml downloads");

// And the safe cases still render as themselves.
const ok = render("**bold** and a [link](https://example.com/)");
equal(ok.includes("<strong>bold</strong>"), true, "markdown still renders");
equal(ok.includes('href="https://example.com/"'), true, "a normal link keeps its target");

declare function require<T = unknown>(name: string): T;

async function blobTransportChecks() {
  const { Window } = require<{ Window: new (options: { url: string }) => TestWindow }>("happy-dom");
  const win = new Window({ url: "http://localhost/wiki/movie.mp4" });
  for (const name of ["window", "document", "navigator", "Node", "HTMLElement", "Event", "location", "sessionStorage", "localStorage"] as const) {
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true });
  }
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true, configurable: true });
  const loader = require<{ prototype: { require: (this: unknown, id: string) => unknown } }>("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.call(this, id);
  };
  const React = require<typeof ReactModule>("react");
  const { createRoot } = require<typeof ReactDomClient>("react-dom/client");
  const Wiki = require<typeof WikiModule>("../src/features/wiki/Wiki").default;
  const { LocaleProvider } = require<typeof LocaleModule>("../src/lib/locale");
  const { setSessionKey } = require<typeof SessionModule>("../src/lib/sessionKey");
  const { allowPrivateReads, pausePrivateReads, resetPrivateSession } = require<typeof PrivateSessionModule>("../src/lib/privateSession");
  const { wikiBlob } = require<typeof WikiApiModule>("../src/features/wiki/api");
  setSessionKey("private-tab-key");
  allowPrivateReads();

  const objects = new Map<string, Blob>();
  const revoked: string[] = [];
  let nextUrl = 0;
  const originalCreate = URL.createObjectURL;
  const originalRevoke = URL.revokeObjectURL;
  URL.createObjectURL = (blob) => {
    if (!(blob instanceof Blob)) throw new Error("Wiki local representations must be blobs");
    const url = `blob:http://localhost/${++nextUrl}`;
    objects.set(url, blob);
    return url;
  };
  URL.revokeObjectURL = (url) => { revoked.push(url); objects.delete(url); };
  const readers: TestReader[] = [];
  class TestReader {
    static LOADING = 1;
    readyState = 0;
    result: string | null = null;
    onload: (() => void) | null = null;
    onerror: (() => void) | null = null;
    readAsDataURL(_blob: Blob) { this.readyState = 1; readers.push(this); }
    abort() { this.readyState = 2; }
  }
  Object.defineProperty(globalThis, "FileReader", { value: TestReader, configurable: true });
  const files: Record<string, string> = {
    "movie.mp4": "video/mp4", "notes.pdf": "application/pdf", "photo.png": "image/png",
    "unsafe.mp4": "text/html", "unsafe.html": "text/html", "late.pdf": "application/pdf",
  };
  const calls: { url: string; init?: RequestInit }[] = [];
  let releaseBody: ((blob: Blob) => void) | null = null;
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async (input, init) => {
    const url = new URL(String(input), "http://localhost");
    calls.push({ url: String(input), init });
    const path = url.searchParams.get("path") ?? "";
    if (url.pathname === "/api/wiki/ls") {
      const entries = Object.entries(files).map(([name, mime]) => ({ path: name, name, mime, kind: "file" }));
      return new Response(JSON.stringify({ path, kind: path ? "file" : "dir", entries: path ? entries.filter((entry) => entry.path === path) : entries }), { headers: { "Content-Type": "application/json" } });
    }
    if (url.pathname === "/api/wiki/file") {
      const response = new Response("private file bytes", { headers: { "Content-Type": files[path] } });
      if (path === "late.pdf") response.blob = () => new Promise<Blob>((resolve) => { releaseBody = resolve; });
      return response;
    }
    throw new Error(`Unexpected wiki request: ${url}`);
  };
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const renderWiki = async (path: string | null) => {
    await React.act(async () => {
      root.render(React.createElement(LocaleProvider, null, React.createElement(Wiki, {
        route: { screen: "wiki", mode: "browse", path, query: null },
        navHref: () => "/wiki", readOnly: true, actor: "operator", onToast: () => {},
      })));
    });
  };
  try {
    await renderWiki("movie.mp4");
    const video = host.querySelector("video");
    equal(video?.getAttribute("src")?.startsWith("blob:"), true, "video uses credentialed local bytes");
    const videoUrl = video!.getAttribute("src")!;
    const download = host.querySelector<HTMLAnchorElement>("a[download]")!;
    equal(download.download, "movie.mp4", "the blob download has a filename");
    equal(objects.get(download.getAttribute("href")!)?.type, "application/octet-stream", "download content cannot open as active HTML");
    const request = calls.find((call) => call.url.includes("/api/wiki/file"));
    equal(new Headers(request?.init?.headers).get("X-Cadence-Session"), "private-tab-key", "file fetch carries the tab header");
    equal(calls.some((call) => call.url.includes("private-tab-key")), false, "credentials never enter URLs");

    await renderWiki("notes.pdf");
    equal(revoked.includes(videoUrl), true, "changing files revokes the old preview");
    const frame = host.querySelector("iframe")!;
    equal(frame.getAttribute("src")?.startsWith("blob:"), true, "PDF uses a local blob");
    equal(frame.getAttribute("sandbox"), "", "PDF retains an opaque scriptless sandbox");
    const pdfUrl = frame.getAttribute("src")!;
    await React.act(async () => pausePrivateReads());
    equal(revoked.includes(pdfUrl), true, "pausing reads revokes the PDF immediately");
    equal(host.querySelector("iframe"), null, "paused private content disappears without another fetch");
    allowPrivateReads();

    await renderWiki("unsafe.mp4");
    equal(host.querySelector("video"), null, "a misleading video extension cannot inline active MIME");
    equal(host.querySelector("iframe"), null, "unapproved content cannot become a frame");
    equal(host.querySelector<HTMLAnchorElement>("a[download]")?.download, "unsafe.mp4", "unsupported MIME still downloads safely");
    await renderWiki("unsafe.html");
    equal(host.querySelector("iframe,video,img"), null, "HTML remains download-only");

    await renderWiki("photo.png");
    const reader = readers[readers.length - 1];
    equal(reader.readyState, 1, "image encoding starts after credentialed fetch");
    await React.act(async () => {
      reader.result = "data:image/png;base64,cHJpdmF0ZQ==";
      reader.onload?.();
    });
    equal(host.querySelector("img")?.getAttribute("src"), "data:image/png;base64,cHJpdmF0ZQ==", "images retain their CSP-compatible data transport");
    await React.act(async () => resetPrivateSession());
    equal(host.querySelector("img"), null, "identity reset drops decoded images even when the key stays identical");
    await React.act(async () => reader.onload?.());
    equal(host.querySelector("img"), null, "late image callbacks cannot restore previous-session bytes");
    allowPrivateReads();

    const before = nextUrl;
    const pending = wikiBlob("late.pdf").then(() => false, () => true);
    await Promise.resolve(); await Promise.resolve();
    equal(typeof releaseBody, "function", "the private blob body is pending after headers");
    resetPrivateSession();
    (releaseBody as unknown as (blob: Blob) => void)(new Blob(["old PDF"], { type: "application/pdf" }));
    equal(await pending, true, "decoded bodies from an obsolete identity are refused");
    equal(nextUrl, before, "obsolete response bytes never become object URLs");
    allowPrivateReads();

    await renderWiki(null);
    const actions = host.querySelector<HTMLButtonElement>('button[aria-label="unsafe.html actions"]')!;
    await React.act(async () => actions.click());
    const menu = host.querySelector('[role="group"][aria-label="unsafe.html actions"]')!;
    const menuDownload = Array.from(menu.querySelectorAll("button")).find((button) => button.textContent === "Download")!;
    const originalClick = win.HTMLAnchorElement.prototype.click;
    const clicked: { href: string; name: string; type?: string }[] = [];
    win.HTMLAnchorElement.prototype.click = function (this: HTMLAnchorElement) {
      clicked.push({ href: this.href, name: this.download, type: objects.get(this.href)?.type });
    };
    await React.act(async () => menuDownload.click());
    win.HTMLAnchorElement.prototype.click = originalClick;
    equal(clicked[0]?.name, "unsafe.html", "folder file downloads retain their filename");
    equal(clicked[0]?.href.startsWith("blob:"), true, "folder download fetches bytes instead of navigating to a private API");
    equal(clicked[0]?.type, "application/octet-stream", "folder HTML downloads are inert too");
    await React.act(async () => root.unmount());
    equal(objects.size, 0, "unmount revokes all preview and pending download URLs");
  } finally {
    await React.act(async () => root.unmount());
    globalThis.fetch = originalFetch;
    URL.createObjectURL = originalCreate;
    URL.revokeObjectURL = originalRevoke;
    loader.prototype.require = originalRequire;
    host.remove();
  }
}

void blobTransportChecks().then(() => console.log("wiki xss and private blob checks passed")).catch((error: unknown) => {
  console.error(error);
  (globalThis as typeof globalThis & { process: { exitCode: number } }).process.exitCode = 1;
});
