import type * as HappyDOM from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDOM from "react-dom/client";
import type * as Locale from "../src/lib/locale";
import type * as Preview from "../src/features/app-shell/chat/conversationPreview";
import type * as ThreadViewModule from "../src/features/home/ThreadView";
import type * as SmoothedPreviewModule from "../src/features/app-shell/chat/useSmoothedPreview";
import type { ThreadEntry, ThreadItem } from "../src/features/home/thread";

declare function require<T>(name: string): T;

const { Window } = require<typeof HappyDOM>("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const key of [
  "window",
  "document",
  "navigator",
  "Node",
  "Element",
  "HTMLElement",
  "HTMLInputElement",
  "HTMLTextAreaElement",
  "HTMLButtonElement",
  "HTMLFormElement",
  "MutationObserver",
  "getComputedStyle",
  "localStorage",
  "sessionStorage",
] as const) {
  Object.defineProperty(globalThis, key, {
    value: key === "window" ? win : win[key],
    configurable: true,
  });
}

let reducedMotion = false;
Object.defineProperty(win, "matchMedia", {
  value: (query: string) => ({
    matches: query.includes("prefers-reduced-motion") ? reducedMotion : false,
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
  }),
  configurable: true,
});

const frameQueue: Array<{ id: number; cb: (ts: number) => void }> = [];
let nextFrameId = 1;
let frameClock = 0;
Object.defineProperty(globalThis, "requestAnimationFrame", {
  value: (cb: (ts: number) => void) => {
    const id = nextFrameId++;
    frameQueue.push({ id, cb });
    return id;
  },
  configurable: true,
});
Object.defineProperty(globalThis, "cancelAnimationFrame", {
  value: (id: number) => {
    const idx = frameQueue.findIndex((item) => item.id === id);
    if (idx >= 0) frameQueue.splice(idx, 1);
  },
  configurable: true,
});
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

const loader = require<{ prototype: { require: (this: unknown, id: string) => unknown } }>("module");
const originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react" || id === "remark-gfm") return { HugeiconsIcon: () => null };
  if (id === "react-markdown") {
    return {
      __esModule: true,
      default: ({ children }: { children?: string }) =>
        React.createElement("p", null, children ?? ""),
    };
  }
  return originalRequire.call(this, id);
};

const React = require<typeof ReactModule>("react");
const { createRoot } = require<typeof ReactDOM>("react-dom/client");
const { LocaleProvider } = require<typeof Locale>("../src/lib/locale");
const { setConversationPreview } = require<typeof Preview>(
  "../src/features/app-shell/chat/conversationPreview",
);
const { ThreadTurn } = require<typeof ThreadViewModule>("../src/features/home/ThreadView");
const { advancePreviewReveal, firstPreviewSlice } = require<typeof SmoothedPreviewModule>(
  "../src/features/app-shell/chat/useSmoothedPreview",
);
loader.prototype.require = originalRequire;

function ok(cond: boolean, message: string): void {
  if (!cond) throw new Error(`assertion failed: ${message}`);
}

const flushFrames = async (steps: number, dtMs = 16) => {
  for (let i = 0; i < steps && frameQueue.length > 0; i += 1) {
    const batch = frameQueue.splice(0, frameQueue.length);
    frameClock += dtMs;
    await React.act(async () => {
      for (const item of batch) item.cb(frameClock);
    });
  }
};

async function main() {
  // 1. Pure helpers: word-aligned opening slice, surrogate safety, multi-frame catch-up, immediate redaction snap.
  const longChunk = "Hong Kong is a dense, fast-moving city where skyline and harbor meet.";
  const opening = firstPreviewSlice(longChunk);
  ok(
    opening.length >= 8 && opening.length < longChunk.length && opening.trim() !== "",
    `opening slice reveals a concise prefix (${JSON.stringify(opening)})`,
  );

  const emoji = "Hello \u{1F680}\u{1F30F} world from Hong Kong";
  const emojiSlice = firstPreviewSlice(emoji);
  for (let i = 0; i < emojiSlice.length; i++) {
    const code = emojiSlice.charCodeAt(i);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = emojiSlice.charCodeAt(i + 1);
      ok(next >= 0xdc00 && next <= 0xdfff, "firstPreviewSlice never leaves a lone high surrogate");
    }
  }

  let stepped = opening;
  let hops = 0;
  while (stepped !== longChunk && hops < 40) {
    const next = advancePreviewReveal(stepped, longChunk, 16);
    ok(next.length > stepped.length && longChunk.startsWith(next), "advancePreviewReveal progresses monotonically");
    stepped = next;
    hops += 1;
  }
  ok(stepped === longChunk && hops >= 2, `67-char burst revealed over ${hops} frames instead of 1 jump`);
  ok(
    advancePreviewReveal("Safe prefix that got redacted", "Safe prefix", 16) === "Safe prefix",
    "shrink/redaction snaps immediately without animating backward",
  );

  // 2. DOM integration on ThreadTurn: immediate prefix + live caret -> multi-frame reveal -> instant committed entry.
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const convId = "conv-smooth-preview-test";
  const answerEntry: ThreadEntry = {
    seq: 2,
    role: "assistant",
    kind: "turn_result",
    text: longChunk,
    created: new Date().toISOString(),
  };

  await React.act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(ThreadTurn, {
          items: [],
          running: true,
          previewConversation: convId,
        }),
      ),
    );
  });

  // Deliver a 67-char preview chunk and drain conversationPreview's rAF notify.
  setConversationPreview(convId, longChunk);
  await flushFrames(1);

  const previewNode = () => host.querySelector('[data-kind="preview"]') as HTMLElement | null;
  ok(previewNode() !== null, "preview node mounts immediately on first frame");
  ok(
    previewNode()?.getAttribute("data-streaming") === "true",
    "preview carries data-streaming=true while generating",
  );
  const firstPainted = previewNode()?.textContent ?? "";
  ok(
    firstPainted.length >= 8 && firstPainted.length < longChunk.length,
    `first painted DOM preview is partial (${JSON.stringify(firstPainted)}), not a full jump`,
  );

  // Advance animation frames until full chunk is smoothly revealed.
  await flushFrames(35, 16);
  ok(previewNode()?.textContent === longChunk, "preview smoothly finishes revealing the burst");

  // Reduced-motion mode paints an incoming chunk instantaneously in one render.
  reducedMotion = true;
  const secondBurst = "Immediate reduced motion output with no character reveal lag.";
  setConversationPreview(convId, secondBurst);
  await flushFrames(1);
  ok(
    previewNode()?.textContent === secondBurst,
    "prefers-reduced-motion: reduce bypasses frame stepping and snaps to full text",
  );
  reducedMotion = false;

  // Committed answer settles immediately without waiting on reveal animation.
  const settledItems: ThreadItem[] = [
    { key: "ans-1", type: "answer", entry: answerEntry },
  ];
  await React.act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(ThreadTurn, {
          items: settledItems,
          running: false,
          previewConversation: convId,
        }),
      ),
    );
  });
  setConversationPreview(convId, null);
  ok(previewNode() === null, "preview clears cleanly once the committed answer renders");
  ok(host.textContent?.includes(longChunk), "committed answer appears immediately");

  await React.act(async () => {
    root.unmount();
  });
  await win.happyDOM.close();
  console.log("smoothed preview checks passed");
}

void main();
