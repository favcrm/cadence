/**
 * CAD-2272: mounted tests for `ScreenAudioModal` covering quote preflight
 * refusal mapping (AOS-572 codes), upload segment transcription + idempotent
 * `request_id` preservation on retry, and AbortSignal cancellation.
 */
import type * as HappyDom from "happy-dom";
import type * as ReactNs from "react";
import type * as ReactDomClient from "react-dom/client";
import type * as LocaleModule from "../src/lib/locale";
import type ScreenAudioModalType from "../src/features/workspace-apps/screen/ScreenAudioModal";
import type * as AudioNormalizeModule from "../src/features/workspace-apps/screen/audioNormalize";
import type { ActionResult } from "../src/features/workspace-apps/screen/screenProtocol";

declare function require(name: string): unknown;
declare const process: { exit(code: number): never };
export {};

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

type ModuleLoader = {
  prototype: { require: (this: unknown, id: string) => unknown };
};

function defaultExport(mod: unknown): unknown {
  assert(mod && typeof mod === "object" && "default" in mod, "module must have default export");
  return Reflect.get(mod, "default");
}

function makeCanonicalWavFile(name: string, seconds: number, lastModified: number): File {
  const sampleRate = 16_000;
  const frames = Math.floor(seconds * sampleRate);
  const samples = new Float32Array(frames);
  for (let i = 0; i < frames; i++) {
    samples[i] = Math.sin((2 * Math.PI * 440 * i) / sampleRate) * 0.25;
  }
  const { encodeWav16kMonoPcm16 } = require(
    "../src/features/workspace-apps/screen/audioNormalize",
  ) as typeof AudioNormalizeModule;
  const wavBytes = encodeWav16kMonoPcm16(samples);
  const copy = new Uint8Array(wavBytes.byteLength);
  copy.set(wavBytes);
  return new File([copy.buffer], name, { type: "audio/wav", lastModified });
}

async function flush(React: typeof ReactNs, rounds = 6): Promise<void> {
  for (let i = 0; i < rounds; i++) {
    await React.act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 5));
    });
  }
}

async function main() {
  const { Window } = require("happy-dom") as typeof HappyDom;
  const win = new Window({ url: "http://localhost/" });
  for (const name of [
    "window",
    "document",
    "Node",
    "Element",
    "HTMLElement",
    "HTMLDialogElement",
    "HTMLInputElement",
    "HTMLSelectElement",
    "HTMLTextAreaElement",
    "HTMLButtonElement",
    "SVGElement",
    "navigator",
    "MutationObserver",
    "ResizeObserver",
    "Event",
    "MouseEvent",
    "KeyboardEvent",
    "File",
    "Blob",
    "location",
    "history",
    "localStorage",
    "sessionStorage",
  ] as const) {
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : Reflect.get(win, name),
      configurable: true,
      writable: true,
    });
  }
  for (const name of [
    "addEventListener",
    "removeEventListener",
    "requestAnimationFrame",
    "cancelAnimationFrame",
  ] as const) {
    Object.defineProperty(globalThis, name, {
      value: (Reflect.get(win, name) as (...args: unknown[]) => unknown).bind(win),
      configurable: true,
      writable: true,
    });
  }
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

  // Ensure `<dialog>` methods exist cleanly in HappyDOM.
  const dialogProto = Reflect.get(win, "HTMLDialogElement")?.prototype as
    | { showModal?: () => void; close?: () => void; open?: boolean }
    | undefined;
  if (dialogProto) {
    if (typeof dialogProto.showModal !== "function") {
      dialogProto.showModal = function (this: HTMLDialogElement) {
        this.setAttribute("open", "");
      };
    }
    if (typeof dialogProto.close !== "function") {
      dialogProto.close = function (this: HTMLDialogElement) {
        this.removeAttribute("open");
      };
    }
  }

  const loader = require("module") as ModuleLoader;
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, [id]);
  };

  const React = require("react") as typeof ReactNs;
  const { createRoot } = require("react-dom/client") as typeof ReactDomClient;
  const { LocaleProvider } = require("../src/lib/locale") as typeof LocaleModule;
  const ScreenAudioModal = defaultExport(
    require("../src/features/workspace-apps/screen/ScreenAudioModal"),
  ) as typeof ScreenAudioModalType;

  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);

  const fetchCalls: { url: string; body: Record<string, unknown> }[] = [];
  let quoteAttempt = 0;
  let transcribeAttempt = 0;

  Object.defineProperty(globalThis, "fetch", {
    configurable: true,
    writable: true,
    value: async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      const url = typeof input === "string" ? input : input.toString();
      const parsed = typeof init?.body === "string" ? (JSON.parse(init.body) as Record<string, unknown>) : {};
      fetchCalls.push({ url, body: parsed });

      if (url === "/api/app-screen-audio/quote") {
        quoteAttempt += 1;
        if (quoteAttempt === 1) {
          return new Response(
            JSON.stringify({ code: "insufficient_funds", error: "Insufficient workspace funds" }),
            { status: 402, headers: { "Content-Type": "application/json" } },
          );
        }
        return new Response(
          JSON.stringify({ available: true, max_duration_seconds: 60, unavailable_reason: null }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }

      if (url === "/api/app-screen-audio/transcribe") {
        transcribeAttempt += 1;
        if (transcribeAttempt === 1) {
          return new Response(
            JSON.stringify({ code: "idempotency_in_progress", error: "Segment still in progress" }),
            { status: 409, headers: { "Content-Type": "application/json" } },
          );
        }
        return new Response(
          JSON.stringify({
            text: "Confirmed Harbour City launch timeline for next Tuesday.",
            duration_ms: 1000,
            repeated: false,
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }

      return new Response("Not found", { status: 404 });
    },
  });

  // 1. Quote 402 (`insufficient_funds`) -> Retry -> Upload WAV -> 409 retry reuses request_id -> Use transcript
  const results: ActionResult[] = [];
  const abort1 = new AbortController();
  const heldEpoch = Date.parse("2026-11-13T04:00:00.000Z");

  await React.act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(ScreenAudioModal, {
          key: "modal-1",
          initialMode: "upload",
          actionToken: "tok-audio-test-1",
          signal: abort1.signal,
          onResolve: (r: ActionResult) => {
            results.push(r);
          },
        }),
      ),
    );
  });
  await flush(React);

  assert(
    (host.textContent ?? "").includes("Workspace credits are insufficient for audio transcription."),
    "402 insufficient_funds quote refusal maps to localized workspace credits copy",
  );

  // Click Retry on quote alert
  const retryQuoteBtn = Array.from(host.querySelectorAll("button")).find(
    (b) => b.textContent?.trim() === "Retry",
  );
  assert(retryQuoteBtn, "Retry button must appear when quote preflight fails");
  await React.act(async () => {
    retryQuoteBtn.click();
  });
  await flush(React);

  const fileInput = host.querySelector('input[type="file"]') as HTMLInputElement | null;
  assert(fileInput, "Upload mode renders file input once quote is ready");

  const wavFile = makeCanonicalWavFile("memo.wav", 1, heldEpoch);
  await React.act(async () => {
    Object.defineProperty(fileInput, "files", {
      configurable: true,
      value: [wavFile],
    });
    fileInput.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await flush(React, 10);

  assert(
    (host.textContent ?? "").includes("Speech transcription is busy right now. Retry the failed segment."),
    "409 idempotency_in_progress maps to busy retry copy",
  );

  const firstTranscribeCalls = fetchCalls.filter((c) => c.url === "/api/app-screen-audio/transcribe");
  assert(firstTranscribeCalls.length === 1, "first transcribe attempt ran once");
  const firstRequestId = firstTranscribeCalls[0]!.body.request_id;
  assert(
    typeof firstRequestId === "string" && /^aud_[a-f0-9]{24}_0$/.test(firstRequestId),
    "request_id follows aud_<session>_<index> grammar",
  );
  assert(
    Object.keys(firstTranscribeCalls[0]!.body).sort().join(",") ===
      "action_token,audio_base64,request_id",
    "transcribe payload sends exact closed keys when language hint is omitted",
  );

  // Retry failed segments must reuse the exact same request_id
  const retrySegBtn = Array.from(host.querySelectorAll("button")).find(
    (b) => b.textContent?.trim() === "Retry failed segments",
  );
  assert(retrySegBtn, "Retry failed segments button is rendered");
  await React.act(async () => {
    retrySegBtn.click();
  });
  await flush(React, 10);

  const allTranscribeCalls = fetchCalls.filter((c) => c.url === "/api/app-screen-audio/transcribe");
  assert(allTranscribeCalls.length === 2, "second transcribe attempt ran");
  assert(
    allTranscribeCalls[1]!.body.request_id === firstRequestId,
    "segment retry preserves the exact same request_id for idempotency",
  );

  const useBtn = Array.from(host.querySelectorAll("button")).find(
    (b) => b.textContent?.trim() === "Use transcript",
  );
  assert(useBtn && !useBtn.disabled, "Use transcript button is enabled after segment succeeds");
  await React.act(async () => {
    useBtn.click();
  });

  assert(results.length === 1 && results[0]!.ok === true, "Use transcript resolves ok: true");
  assert(
    JSON.stringify(results[0]!.data) ===
      JSON.stringify({
        transcript: "Confirmed Harbour City launch timeline for next Tuesday.",
        duration_seconds: 1,
        held_at: "2026-11-13T04:00:00.000Z",
        source: "upload",
      }),
    "resolved ScreenAudioCapture matches exact expected payload",
  );

  // 2. AbortSignal cancellation resolves code="cancelled"
  const abort2 = new AbortController();
  const cancelResults: ActionResult[] = [];
  await React.act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(ScreenAudioModal, {
          key: "modal-2",
          initialMode: "record",
          actionToken: "tok-audio-test-2",
          signal: abort2.signal,
          onResolve: (r: ActionResult) => {
            cancelResults.push(r);
          },
        }),
      ),
    );
  });
  await flush(React);
  await React.act(async () => {
    abort2.abort();
  });
  assert(
    cancelResults.length === 1 &&
      cancelResults[0]!.ok === false &&
      cancelResults[0]!.refusal.code === "cancelled",
    "AbortSignal abort resolves refusal code='cancelled'",
  );

  await React.act(async () => {
    root.unmount();
  });
  console.log("screenAudioModal checks passed");
}

void main().catch((e: unknown) => {
  console.error(e);
  process.exit(1);
});
