import { encodeWav16kMonoPcm16 } from "../src/features/workspace-apps/screen/audioNormalize";
import {
  clampTranscriptUtf8,
  createScreenAudioSession,
  normalizeRefusalCode,
  readErrorCode,
} from "../src/features/workspace-apps/screen/screenAudioSession";
import type { ScreenAudioSessionSnapshot } from "../src/features/workspace-apps/screen/screenProtocol";

declare const process: { exit(code: number): never };

function check(condition: unknown, label: string): asserts condition {
  if (!condition) throw new Error(`screenAudioSession test failed: ${label}`);
}

function makeCanonicalWavBuffer(seconds: number): ArrayBuffer {
  const sampleRate = 16_000;
  const frames = Math.floor(seconds * sampleRate);
  const samples = new Float32Array(frames);
  for (let i = 0; i < frames; i++) {
    samples[i] = Math.sin((2 * Math.PI * 440 * i) / sampleRate) * 0.3;
  }
  const wav = encodeWav16kMonoPcm16(samples);
  const out = new Uint8Array(wav.byteLength);
  out.set(wav);
  return out.buffer;
}

void (async () => {
  const prevFetch = globalThis.fetch;
  const prevAudioContext = (globalThis as { AudioContext?: unknown }).AudioContext;
  const prevNavigator = Object.getOwnPropertyDescriptor(globalThis, "navigator");

  try {
    const fetchCalls: Array<{ url: string; body: Record<string, unknown> }> = [];
    let quoteAttempt = 0;
    let transcribeMode: "ok" | "fail_once" = "ok";
    let failOnceConsumed = false;

    globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      const url = typeof input === "string" ? input : input.toString();
      const body = typeof init?.body === "string" ? (JSON.parse(init.body) as Record<string, unknown>) : {};
      fetchCalls.push({ url, body });

      if (url === "/api/app-screen-audio/quote") {
        quoteAttempt += 1;
        if (quoteAttempt === 1) {
          return new Response(JSON.stringify({ code: "insufficient_funds" }), {
            status: 402,
            headers: { "Content-Type": "application/json" },
          });
        }
        return new Response(
          JSON.stringify({ available: true, max_duration_seconds: 60, unavailable_reason: null }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }

      if (url === "/api/app-screen-audio/transcribe") {
        if (transcribeMode === "fail_once" && !failOnceConsumed) {
          failOnceConsumed = true;
          return new Response(JSON.stringify({ code: "idempotency_in_progress" }), {
            status: 409,
            headers: { "Content-Type": "application/json" },
          });
        }
        return new Response(
          JSON.stringify({
            text: `Transcribed ${String(body.request_id)}`,
            duration_ms: 1000,
            repeated: false,
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      return new Response("Not found", { status: 404 });
    }) as typeof fetch;

    let micTrackStops = 0;
    let closedAudioContexts = 0;
    let processorFn: ((ev: { inputBuffer: { getChannelData: (ch: number) => Float32Array } }) => void) | null = null;

    Object.defineProperty(globalThis, "navigator", {
      configurable: true,
      value: {
        mediaDevices: {
          async getUserMedia() {
            return {
              getTracks: () => [{ stop: () => { micTrackStops += 1; } }],
            };
          },
        },
      },
    });

    class MockAudioContext {
      sampleRate = 16_000;
      state = "running";
      destination = {};
      createMediaStreamSource() {
        return { connect() {}, disconnect() {} };
      }
      createGain() {
        return { gain: { value: 1 }, connect() {}, disconnect() {} };
      }
      createScriptProcessor() {
        const node = {
          onaudioprocess: null as typeof processorFn,
          connect() {},
          disconnect() {},
        };
        Object.defineProperty(node, "onaudioprocess", {
          get: () => processorFn,
          set: (fn) => { processorFn = fn; },
        });
        return node;
      }
      async close() {
        closedAudioContexts += 1;
      }
    }
    (globalThis as { AudioContext?: unknown }).AudioContext = MockAudioContext;

    const events: ScreenAudioSessionSnapshot[] = [];
    const mountAbort = new AbortController();
    const callAbort = new AbortController();
    const session = createScreenAudioSession({
      actionToken: "f".repeat(64),
      signal: mountAbort.signal,
      getLocale: () => "en",
      onEvent: (snap) => events.push(snap),
    });

    // 1. Quote preflight 402 refusal transitions phase to error.
    const refusedStart = await session.dispatch(
      { v: 2, op: "audio-session", id: "s1", action: "start" },
      callAbort.signal,
    );
    check(!refusedStart.ok && refusedStart.refusal.code === "insufficient_funds", "402 quote maps to insufficient_funds");
    check(events.at(-1)?.phase === "error", "quote refusal emits error phase");

    // 2. Live recording: start -> audio samples -> pause -> resume -> stop drains tail segment.
    const started = await session.dispatch(
      { v: 2, op: "audio-session", id: "s2", action: "start", language: "zh" },
      callAbort.signal,
    );
    check(started.ok && (started.data as ScreenAudioSessionSnapshot).phase === "recording", "start enters recording phase");

    const oneSecBlock = new Float32Array(16_000).fill(0.35);
    const invokeProcessor = processorFn as
      | ((ev: { inputBuffer: { getChannelData: (ch: number) => Float32Array } }) => void)
      | null;
    invokeProcessor?.({ inputBuffer: { getChannelData: () => oneSecBlock } });
    check(events.some((e) => e.level > 0 && e.peak >= e.level), "live meter emits integer level and peak");

    const paused = await session.dispatch(
      { v: 2, op: "audio-session", id: "s3", action: "pause" },
      callAbort.signal,
    );
    check(paused.ok && (paused.data as ScreenAudioSessionSnapshot).phase === "paused", "pause transitions to paused");

    const resumed = await session.dispatch(
      { v: 2, op: "audio-session", id: "s4", action: "resume" },
      callAbort.signal,
    );
    check(resumed.ok && (resumed.data as ScreenAudioSessionSnapshot).phase === "recording", "resume returns to recording");

    const stopped = await session.dispatch(
      { v: 2, op: "audio-session", id: "s5", action: "stop" },
      callAbort.signal,
    );
    check(stopped.ok, "stop resolves ok after draining tail segment");
    const stopSnap = stopped.data as ScreenAudioSessionSnapshot;
    check(stopSnap.phase === "ready" && stopSnap.segments_done === 1 && stopSnap.transcript.startsWith("Transcribed aud_"),
      "stop drains tail segment into ready transcript");
    check(micTrackStops === 1 && closedAudioContexts === 1, "stop frees microphone track and AudioContext");

    const firstLiveTranscribe = fetchCalls.find((c) => c.url === "/api/app-screen-audio/transcribe");
    check(firstLiveTranscribe?.body.language === "zh", "language hint forwarded to transcribe endpoint");

    // 3. Inline upload with last_modified + retry preserving idempotent request_id.
    transcribeMode = "fail_once";
    failOnceConsumed = false;
    const heldMs = Date.parse("2026-11-12T08:30:00.000Z");
    const uploadRes = await session.dispatch(
      {
        v: 2,
        op: "audio-session",
        id: "u1",
        action: "upload",
        bytes: makeCanonicalWavBuffer(1),
        mime_type: "audio/wav",
        file_name: "standup.wav",
        last_modified: heldMs,
      },
      callAbort.signal,
    );
    check(uploadRes.ok, "upload resolves snapshot even when one segment needs retry");
    const failedSnap = uploadRes.data as ScreenAudioSessionSnapshot;
    check(
      failedSnap.phase === "ready" &&
        failedSnap.segments_failed === 1 &&
        failedSnap.held_at === "2026-11-12T08:30:00.000Z",
      "upload preserves held_at from last_modified and reports failed segment",
    );
    const uploadCallsBeforeRetry = fetchCalls.filter((c) => c.url === "/api/app-screen-audio/transcribe");
    const uploadReqId = uploadCallsBeforeRetry.at(-1)?.body.request_id;
    check(typeof uploadReqId === "string" && /^aud_[a-f0-9]{24}_0$/.test(uploadReqId), "upload request_id matches grammar");

    const retried = await session.dispatch(
      { v: 2, op: "audio-session", id: "u2", action: "retry" },
      callAbort.signal,
    );
    check(retried.ok, "retry resolves ok");
    const retrySnap = retried.data as ScreenAudioSessionSnapshot;
    check(
      retrySnap.segments_failed === 0 &&
        retrySnap.segments_done === 1 &&
        retrySnap.transcript === `Transcribed ${uploadReqId}`,
      "retry completes failed segment and clears failure state",
    );
    const uploadCallsAfterRetry = fetchCalls.filter((c) => c.url === "/api/app-screen-audio/transcribe");
    check(uploadCallsAfterRetry.at(-1)?.body.request_id === uploadReqId, "retry reuses exact request_id");

    // 0. Refusal code normalization & UTF-8 transcript byte bound.
    check(normalizeRefusalCode("SPEECH-UNAVAILABLE") === "speech_unavailable", "hyphenated uppercase code normalizes");
    check(normalizeRefusalCode("", "unavailable") === "unavailable", "empty string code falls back");
    check(normalizeRefusalCode("503_bad_gateway") === "e_503_bad_gateway", "digit-prefixed code receives letter prefix");
    check(readErrorCode({ code: "" }) === null, "readErrorCode ignores empty string");
    check(readErrorCode({ error: { code: "RATE-LIMITED" } }) === "rate_limited", "readErrorCode normalizes nested code");
    const cjkHeavy = "會".repeat(90_000); // 270,000 UTF-8 bytes (> 240 KiB)
    const clampedCjk = clampTranscriptUtf8(cjkHeavy);
    check(new TextEncoder().encode(clampedCjk).length <= 240 * 1024, "clampTranscriptUtf8 enforces 240 KiB UTF-8 bound");

    // 4. Mount abort during active live recording stops hardware mic track immediately.
    const live2 = await session.dispatch(
      { v: 2, op: "audio-session", id: "s6", action: "start" },
      callAbort.signal,
    );
    const stopsBeforeAbort: number = micTrackStops;
    check(live2.ok && stopsBeforeAbort === 1, "second live session acquires mic");
    mountAbort.abort();
    await Promise.resolve();
    const stopsAfterAbort: number = micTrackStops;
    const closesAfterAbort: number = closedAudioContexts;
    check(stopsAfterAbort === 2 && closesAfterAbort === 2, "mount abort disposes active microphone and AudioContext");

    // 5. Aborting an individual callSignal mid-transcribe resolves waitForDrain immediately without hanging.
    const mountAbort2 = new AbortController();
    let hangTranscribe = true;
    const origFetch = globalThis.fetch;
    globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      const url = typeof input === "string" ? input : input.toString();
      if (url === "/api/app-screen-audio/transcribe" && hangTranscribe) {
        return await new Promise<Response>((_resolve, reject) => {
          init?.signal?.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")), {
            once: true,
          });
        });
      }
      return origFetch(input, init);
    }) as typeof fetch;

    const session2 = createScreenAudioSession({
      actionToken: "tok_v2_abort_test",
      signal: mountAbort2.signal,
      getLocale: () => "en",
      onEvent: () => {},
    });
    const perCallAbort = new AbortController();
    const pendingUpload = session2.dispatch(
      {
        v: 2,
        op: "audio-session",
        id: "u-abort",
        action: "upload",
        bytes: makeCanonicalWavBuffer(1),
        mime_type: "audio/wav",
      },
      perCallAbort.signal,
    );
    await new Promise((r) => setTimeout(r, 25));
    perCallAbort.abort();
    const abortedUploadRes = await pendingUpload;
    check(
      !abortedUploadRes.ok && abortedUploadRes.refusal.code === "cancelled",
      "aborting upload callSignal during segment transcription resolves cancelled without hanging",
    );
    hangTranscribe = false;
    session2.dispose();

    console.log("screenAudioSession checks pass");
  } finally {
    globalThis.fetch = prevFetch;
    (globalThis as { AudioContext?: unknown }).AudioContext = prevAudioContext;
    if (prevNavigator) {
      Object.defineProperty(globalThis, "navigator", prevNavigator);
    } else {
      delete (globalThis as { navigator?: unknown }).navigator;
    }
  }
})().catch((err) => {
  console.error(err);
  process.exit(1);
});
