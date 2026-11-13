import { translateUiText } from "../../../lib/locale";
import { beginPrivateOperation, type PrivateOperation } from "../../../lib/privateSession";
import { sessionHeaders } from "../../../lib/sessionKey";
import {
  AudioNormalizeError,
  normalizeAudioFile,
  startLiveMicrophoneCapture,
  type LiveMicrophoneCapture,
  type WavAudioChunk,
} from "./audioNormalize";
import {
  AUDIO_TRANSCRIPT_CHARS_MAX,
  type ActionRefusal,
  type ActionResult,
  type AudioSessionRequest,
  type ScreenAudioLanguage,
  type ScreenAudioMode,
  type ScreenAudioSessionPhase,
  type ScreenAudioSessionSnapshot,
} from "./screenProtocol";

interface QuoteReply {
  available: boolean;
  max_duration_seconds: number;
  unavailable_reason: string | null;
}

interface TranscribeReply {
  text: string;
  duration_ms: number;
  repeated: boolean;
}

interface SessionSegment {
  chunk: WavAudioChunk;
  requestId: string;
  status: "queued" | "transcribing" | "done" | "failed";
  text: string;
  errorCode: string | null;
}

const LEVEL_PUSH_INTERVAL_MS = 100;

export function randomAudioSessionId(): string {
  const bytes = new Uint8Array(12);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

const REFUSAL_CODE_RE = /^[a-z][a-z0-9_]{0,63}$/;
const MAX_TRANSCRIPT_UTF8_BYTES = 240 * 1024;
const UTF8_ENCODER = new TextEncoder();

export function normalizeRefusalCode(raw: unknown, fallback = "failed"): string {
  if (typeof raw !== "string") return fallback;
  const trimmed = raw.trim().toLowerCase();
  if (!trimmed) return fallback;
  const cleaned = trimmed.replace(/[^a-z0-9_]+/g, "_").replace(/^_+|_+$/g, "");
  if (!cleaned) return fallback;
  const prefixed = /^[a-z]/.test(cleaned) ? cleaned : `e_${cleaned}`;
  const bounded = prefixed.slice(0, 64).replace(/_+$/g, "");
  return REFUSAL_CODE_RE.test(bounded) ? bounded : fallback;
}

export function clampTranscriptUtf8(raw: string): string {
  const charBounded = raw.slice(0, AUDIO_TRANSCRIPT_CHARS_MAX);
  if (UTF8_ENCODER.encode(charBounded).length <= MAX_TRANSCRIPT_UTF8_BYTES) {
    return charBounded;
  }
  let lo = 0;
  let hi = charBounded.length;
  while (lo < hi) {
    const mid = Math.ceil((lo + hi) / 2);
    if (UTF8_ENCODER.encode(charBounded.slice(0, mid)).length <= MAX_TRANSCRIPT_UTF8_BYTES) {
      lo = mid;
    } else {
      hi = mid - 1;
    }
  }
  return charBounded.slice(0, lo);
}

export function readErrorCode(body: unknown): string | null {
  if (!body || typeof body !== "object" || Array.isArray(body)) return null;
  const record = body as Record<string, unknown>;
  if (typeof record.code === "string") {
    const norm = normalizeRefusalCode(record.code, "");
    return norm || null;
  }
  if (
    record.error &&
    typeof record.error === "object" &&
    !Array.isArray(record.error) &&
    typeof (record.error as Record<string, unknown>).code === "string"
  ) {
    const norm = normalizeRefusalCode((record.error as Record<string, unknown>).code, "");
    return norm || null;
  }
  return null;
}

export function unavailableMessage(
  code: string | null,
  status: number,
  t: (english: string) => string,
): string {
  if (status === 403 || code === "owner_required") {
    return t("Only a workspace owner can run audio transcription.");
  }
  if (status === 404) {
    return t("Audio transcription is not enabled on this board.");
  }
  if (status === 402 || code === "insufficient_funds" || code === "insufficient_balance") {
    return t("Workspace credits are insufficient for audio transcription.");
  }
  if (
    code === "disabled" ||
    code === "speech_unavailable" ||
    code === "unpriced_default" ||
    code === "not_configured" ||
    code === "provider_unavailable"
  ) {
    return t("Speech transcription is not configured for this workspace yet.");
  }
  return t("Audio transcription is currently unavailable. Try again shortly.");
}

export function transcribeFailureMessage(
  code: string | null,
  t: (english: string) => string,
): string {
  if (code === "insufficient_funds" || code === "insufficient_balance") {
    return t("Workspace credits are insufficient to finish transcribing.");
  }
  if (code === "rate_limited" || code === "idempotency_in_progress" || code === "uncertain") {
    return t("Speech transcription is busy right now. Retry the failed segment.");
  }
  if (code === "disabled" || code === "speech_unavailable") {
    return t("Speech transcription is not configured for this workspace yet.");
  }
  if (code === "audio_too_large" || code === "unsupported_audio_format") {
    return t("One audio segment could not be accepted by the speech service.");
  }
  if (code === "owner_required") {
    return t("Only a workspace owner can run audio transcription.");
  }
  return t("Some audio segments did not finish transcribing. Retry to complete.");
}

export function normalizeFailureMessage(
  error: unknown,
  t: (english: string) => string,
): string {
  if (error instanceof AudioNormalizeError) {
    if (error.code === "empty_audio") {
      return t("That audio file is empty.");
    }
    if (error.code === "audio_too_large") {
      return t("That file exceeds the 50 MB audio upload limit.");
    }
    if (error.code === "unsupported_audio_format") {
      return t(
        "Unsupported audio file. Choose an M4A, MP3, WAV, Opus/Ogg, WebM, AAC, FLAC, 3GP, or AMR recording.",
      );
    }
    if (error.code === "duration_too_long") {
      return t("Audio recordings over 2 hours must be split before uploading.");
    }
    return t("This browser could not decode that audio file. Try M4A, MP3, WAV, Ogg, or WebM.");
  }
  return t("Could not read that audio file.");
}

export interface ScreenAudioSessionController {
  dispatch(request: AudioSessionRequest, callSignal: AbortSignal): Promise<ActionResult>;
  dispose(): void;
}

export function createScreenAudioSession({
  actionToken,
  signal: mountSignal,
  getLocale,
  onEvent,
}: {
  actionToken: string;
  signal: AbortSignal;
  getLocale: () => "en" | "zh-TW";
  onEvent: (snapshot: ScreenAudioSessionSnapshot) => void;
}): ScreenAudioSessionController {
  const t = (english: string) => translateUiText(english, getLocale());

  let disposed = false;
  let quoteVerified = false;
  let sessionId = "";
  let mode: ScreenAudioMode = "record";
  let phase: ScreenAudioSessionPhase = "idle";
  let heldAt = "";
  let elapsedSeconds = 0;
  let level = 0;
  let peak = 0;
  let normalizingTotal = 0;
  let language: ScreenAudioLanguage = "";
  let segments: SessionSegment[] = [];
  let lastError: ActionRefusal | null = null;
  let liveRecorder: LiveMicrophoneCapture | null = null;
  let sessionAbort: AbortController | null = null;
  let pumping = false;
  let lastLevelEmitAt = 0;
  let drainWaiters: Array<() => void> = [];

  const buildSnapshot = (): ScreenAudioSessionSnapshot => {
    const done = segments.filter((s) => s.status === "done").length;
    const inFlight = segments.filter((s) => s.status === "queued" || s.status === "transcribing").length;
    const failed = segments.filter((s) => s.status === "failed").length;
    const noSpeech = segments.filter((s) => s.status === "done" && s.text.length === 0).length;
    const total = segments.length > 0 ? segments.length : normalizingTotal;
    const transcript = clampTranscriptUtf8(
      segments
        .filter((s) => s.status === "done" && s.text.length > 0)
        .map((s) => s.text)
        .join("\n"),
    );
    return {
      session_id: sessionId,
      mode,
      phase,
      held_at: heldAt,
      elapsed_seconds: Math.max(0, Math.min(86_400, Math.floor(elapsedSeconds))),
      level: Math.max(0, Math.min(100, Math.round(level))),
      peak: Math.max(0, Math.min(100, Math.round(peak))),
      segments_total: Math.min(10_000, total),
      segments_done: Math.min(10_000, done),
      segments_in_flight: Math.min(10_000, inFlight),
      segments_failed: Math.min(10_000, failed),
      no_speech_segments: Math.min(10_000, noSpeech),
      transcript,
      error: lastError,
    };
  };

  const emit = () => {
    if (disposed || mountSignal.aborted) return;
    onEvent(buildSnapshot());
  };

  const emitMeterIfDue = () => {
    const now = Date.now();
    if (now - lastLevelEmitAt < LEVEL_PUSH_INTERVAL_MS) return;
    lastLevelEmitAt = now;
    emit();
  };

  const hasPendingSegments = () =>
    segments.some((s) => s.status === "queued" || s.status === "transcribing");

  const notifyDrainIfIdle = () => {
    if (hasPendingSegments()) return;
    const waiters = drainWaiters;
    drainWaiters = [];
    for (const wake of waiters) wake();
  };

  const waitForDrain = (activeSessionId: string): Promise<void> => {
    if (!hasPendingSegments() || sessionId !== activeSessionId || disposed) {
      return Promise.resolve();
    }
    return new Promise((resolve) => {
      drainWaiters.push(resolve);
    });
  };

  const abortActiveWork = () => {
    const rec = liveRecorder;
    liveRecorder = null;
    if (rec) void rec.abort();
    const ctrl = sessionAbort;
    sessionAbort = null;
    ctrl?.abort();
    const waiters = drainWaiters;
    drainWaiters = [];
    for (const wake of waiters) wake();
  };

  const dispose = () => {
    if (disposed) return;
    disposed = true;
    abortActiveWork();
  };

  if (mountSignal.aborted) {
    dispose();
  } else {
    mountSignal.addEventListener("abort", dispose, { once: true });
  }

  const ensureQuote = async (activeSignal: AbortSignal): Promise<ActionRefusal | null> => {
    if (quoteVerified) return null;
    let op: PrivateOperation | null = null;
    try {
      op = beginPrivateOperation(activeSignal);
    } catch {
      return {
        code: "unauthorized",
        text: t("Session verification is required before using audio transcription."),
      };
    }
    try {
      const res = await op.fetch("/api/app-screen-audio/quote", {
        method: "POST",
        credentials: "same-origin",
        cache: "no-store",
        headers: {
          "Content-Type": "application/json",
          "X-Cadence-Board": "1",
          ...sessionHeaders(),
        },
        body: JSON.stringify({ action_token: actionToken }),
      });
      const body = (await res.json().catch(() => null)) as QuoteReply | Record<string, unknown> | null;
      if (!res.ok) {
        const code = readErrorCode(body) ?? (res.status === 403 ? "forbidden" : "unavailable");
        return { code, text: unavailableMessage(code, res.status, t) };
      }
      const quote = body as QuoteReply | null;
      if (!quote || typeof quote.available !== "boolean") {
        return {
          code: "unavailable",
          text: t("Audio transcription is currently unavailable. Try again shortly."),
        };
      }
      if (!quote.available) {
        const reason = normalizeRefusalCode(quote.unavailable_reason, "unavailable");
        return { code: reason, text: unavailableMessage(reason, 200, t) };
      }
      quoteVerified = true;
      return null;
    } catch {
      return {
        code: "unavailable",
        text: t("Could not reach the audio transcription service. Check your connection and retry."),
      };
    } finally {
      op.dispose();
    }
  };

  const pumpSegments = () => {
    if (pumping || disposed || mountSignal.aborted) return;
    const next = segments.find((item) => item.status === "queued");
    if (!next) {
      notifyDrainIfIdle();
      return;
    }
    const activeSessionId = sessionId;
    const activeAbort = sessionAbort;
    if (!activeAbort || activeAbort.signal.aborted) {
      notifyDrainIfIdle();
      return;
    }

    pumping = true;
    next.status = "transcribing";
    next.errorCode = null;
    emit();

    void (async () => {
      let op: PrivateOperation | null = null;
      try {
        op = beginPrivateOperation(activeAbort.signal);
        const res = await op.fetch("/api/app-screen-audio/transcribe", {
          method: "POST",
          credentials: "same-origin",
          cache: "no-store",
          headers: {
            "Content-Type": "application/json",
            "X-Cadence-Board": "1",
            ...sessionHeaders(),
          },
          body: JSON.stringify({
            action_token: actionToken,
            request_id: next.requestId,
            audio_base64: next.chunk.base64,
            ...(language ? { language } : {}),
          }),
        });
        const body = (await res.json().catch(() => null)) as TranscribeReply | Record<string, unknown> | null;
        if (disposed || activeAbort.signal.aborted || sessionId !== activeSessionId) return;
        if (!res.ok || !body || typeof (body as TranscribeReply).text !== "string") {
          const code = readErrorCode(body) ?? "failed";
          next.status = "failed";
          next.errorCode = code;
          lastError = { code, text: transcribeFailureMessage(code, t) };
        } else {
          next.status = "done";
          next.text = (body as TranscribeReply).text.trim();
          next.errorCode = null;
          next.chunk = { ...next.chunk, wavBytes: new Uint8Array(0), base64: "" };
          if (!segments.some((s) => s.status === "failed")) {
            lastError = null;
          }
        }
        emit();
      } catch {
        if (!disposed && !activeAbort.signal.aborted && sessionId === activeSessionId) {
          next.status = "failed";
          next.errorCode = "network";
          lastError = {
            code: "network",
            text: t("Network interrupted while transcribing. Retry failed segments."),
          };
          emit();
        }
      } finally {
        op?.dispose();
        if (next.status === "transcribing") {
          next.status = "failed";
          next.errorCode = next.errorCode ?? "cancelled";
        }
        pumping = false;
        if (!disposed && sessionId === activeSessionId && !activeAbort.signal.aborted) {
          if (segments.some((item) => item.status === "queued")) {
            queueMicrotask(pumpSegments);
          } else {
            notifyDrainIfIdle();
          }
        } else {
          const waiters = drainWaiters;
          drainWaiters = [];
          for (const wake of waiters) wake();
        }
      }
    })();
  };

  const isBusyPhase = () =>
    phase === "starting" ||
    phase === "recording" ||
    phase === "paused" ||
    phase === "stopping" ||
    phase === "transcribing";

  const resetSessionState = (next: {
    id: string;
    mode: ScreenAudioMode;
    phase: ScreenAudioSessionPhase;
    heldAt: string;
    normalizingTotal?: number;
    language?: ScreenAudioLanguage;
  }) => {
    sessionId = next.id;
    mode = next.mode;
    phase = next.phase;
    heldAt = next.heldAt;
    elapsedSeconds = 0;
    level = 0;
    peak = 0;
    normalizingTotal = next.normalizingTotal ?? 0;
    if (next.language !== undefined) language = next.language;
    segments = [];
    lastError = null;
  };

  const dispatch = async (
    request: AudioSessionRequest,
    callSignal: AbortSignal,
  ): Promise<ActionResult> => {
    if (disposed || mountSignal.aborted || callSignal.aborted) {
      return { ok: false, refusal: { code: "cancelled", text: "Audio capture was cancelled." } };
    }

    if (request.action === "cancel") {
      abortActiveWork();
      resetSessionState({ id: "", mode: "record", phase: "idle", heldAt: "" });
      emit();
      return { ok: true, data: buildSnapshot() };
    }

    if (request.action === "start") {
      if (callSignal.aborted) {
        return { ok: false, refusal: { code: "cancelled", text: "Audio capture was cancelled." } };
      }
      if (isBusyPhase()) {
        return { ok: false, refusal: { code: "conflict", text: "Audio capture is already active." } };
      }
      abortActiveWork();
      const currentSession = randomAudioSessionId();
      const ctrl = new AbortController();
      const onCallAbort = () => {
        ctrl.abort();
        const waiters = drainWaiters;
        drainWaiters = [];
        for (const wake of waiters) wake();
      };
      callSignal.addEventListener("abort", onCallAbort, { once: true });
      sessionAbort = ctrl;
      resetSessionState({
        id: currentSession,
        mode: "record",
        phase: "starting",
        heldAt: new Date().toISOString(),
        language: request.language ?? "",
      });
      emit();

      try {
        const quoteRefusal = await ensureQuote(ctrl.signal);
        if (disposed || ctrl.signal.aborted || sessionId !== currentSession) {
          return { ok: false, refusal: { code: "cancelled", text: "Audio capture was cancelled." } };
        }
        if (quoteRefusal) {
          phase = "error";
          lastError = quoteRefusal;
          emit();
          return { ok: false, refusal: quoteRefusal };
        }

        const capture = await startLiveMicrophoneCapture({
          onLevel: (lvl) => {
            if (sessionId !== currentSession || phase !== "recording") return;
            const scaled = Math.max(0, Math.min(100, Math.round(lvl * 100)));
            level = scaled;
            peak = scaled >= peak ? scaled : Math.max(scaled, Math.round(peak * 0.88));
            emitMeterIfDue();
          },
          onElapsedMs: (ms) => {
            if (sessionId !== currentSession) return;
            const nextSec = Math.floor(ms / 1000);
            if (nextSec !== elapsedSeconds) {
              elapsedSeconds = nextSec;
              emit();
            }
          },
          onSegment: (chunk) => {
            if (sessionId !== currentSession || ctrl.signal.aborted) return;
            segments.push({
              chunk,
              requestId: `aud_${currentSession}_${chunk.index}`,
              status: "queued",
              text: "",
              errorCode: null,
            });
            emit();
            pumpSegments();
          },
        });
        if (disposed || ctrl.signal.aborted || sessionId !== currentSession) {
          await capture.abort();
          return { ok: false, refusal: { code: "cancelled", text: "Audio capture was cancelled." } };
        }
        liveRecorder = capture;
        heldAt = capture.startedAtIso;
        phase = "recording";
        emit();
        return { ok: true, data: buildSnapshot() };
      } catch (err) {
        if (disposed || ctrl.signal.aborted || sessionId !== currentSession) {
          return { ok: false, refusal: { code: "cancelled", text: "Audio capture was cancelled." } };
        }
        const name = err instanceof DOMException ? err.name : "";
        const text =
          name === "NotAllowedError" || name === "PermissionDeniedError"
            ? t("Microphone access was blocked. Allow microphone permission in your browser settings and try again.")
            : name === "NotFoundError" || name === "DevicesNotFoundError"
              ? t("No microphone was found on this device.")
              : t("Could not start microphone recording in this browser.");
        const refusal: ActionRefusal = { code: "mic_unavailable", text };
        phase = "error";
        lastError = refusal;
        emit();
        return { ok: false, refusal };
      } finally {
        callSignal.removeEventListener("abort", onCallAbort);
      }
    }

    if (request.action === "pause") {
      if (!liveRecorder || phase !== "recording") {
        return { ok: false, refusal: { code: "conflict", text: "Recording is not currently running." } };
      }
      liveRecorder.pause();
      level = 0;
      peak = 0;
      phase = "paused";
      emit();
      return { ok: true, data: buildSnapshot() };
    }

    if (request.action === "resume") {
      if (!liveRecorder || phase !== "paused") {
        return { ok: false, refusal: { code: "conflict", text: "Recording is not paused." } };
      }
      liveRecorder.resume();
      phase = "recording";
      emit();
      return { ok: true, data: buildSnapshot() };
    }

    if (request.action === "stop") {
      if (callSignal.aborted) {
        return { ok: false, refusal: { code: "cancelled", text: "Audio capture was cancelled." } };
      }
      const live = liveRecorder;
      if (!live || (phase !== "recording" && phase !== "paused")) {
        return { ok: false, refusal: { code: "conflict", text: "No active recording to stop." } };
      }
      const currentSession = sessionId;
      if (!sessionAbort || sessionAbort.signal.aborted) {
        sessionAbort = new AbortController();
      }
      const stopAbort = sessionAbort;
      const onStopAbort = () => {
        stopAbort.abort();
        const waiters = drainWaiters;
        drainWaiters = [];
        for (const wake of waiters) wake();
      };
      callSignal.addEventListener("abort", onStopAbort, { once: true });
      liveRecorder = null;
      level = 0;
      peak = 0;
      phase = "stopping";
      emit();

      try {
        const summary = await live.finish();
        if (disposed || sessionId !== currentSession || stopAbort.signal.aborted) {
          return { ok: false, refusal: { code: "cancelled", text: "Audio capture was cancelled." } };
        }
        elapsedSeconds = Math.max(elapsedSeconds, summary.durationSeconds);
        if (summary.totalFrames === 0 && segments.length === 0) {
          const refusal: ActionRefusal = {
            code: "empty_audio",
            text: t("No audio was captured. Start recording again when ready."),
          };
          phase = "error";
          lastError = refusal;
          emit();
          return { ok: false, refusal };
        }

        if (hasPendingSegments()) {
          phase = "transcribing";
          emit();
          pumpSegments();
          await waitForDrain(currentSession);
          if (disposed || sessionId !== currentSession || stopAbort.signal.aborted) {
            return { ok: false, refusal: { code: "cancelled", text: "Audio capture was cancelled." } };
          }
        }
        elapsedSeconds = Math.max(1, elapsedSeconds);
        phase = "ready";
        emit();
        return { ok: true, data: buildSnapshot() };
      } finally {
        callSignal.removeEventListener("abort", onStopAbort);
      }
    }

    if (request.action === "upload") {
      if (callSignal.aborted) {
        return { ok: false, refusal: { code: "cancelled", text: "Audio capture was cancelled." } };
      }
      if (isBusyPhase()) {
        return { ok: false, refusal: { code: "conflict", text: "Audio capture is already active." } };
      }
      abortActiveWork();
      const currentSession = randomAudioSessionId();
      const ctrl = new AbortController();
      const onCallAbort = () => {
        ctrl.abort();
        const waiters = drainWaiters;
        drainWaiters = [];
        for (const wake of waiters) wake();
      };
      callSignal.addEventListener("abort", onCallAbort, { once: true });
      sessionAbort = ctrl;
      resetSessionState({
        id: currentSession,
        mode: "upload",
        phase: "starting",
        heldAt: new Date().toISOString(),
        normalizingTotal: 1,
        language: request.language ?? "",
      });
      emit();

      try {
        const quoteRefusal = await ensureQuote(ctrl.signal);
        if (disposed || ctrl.signal.aborted || sessionId !== currentSession) {
          return { ok: false, refusal: { code: "cancelled", text: "Audio capture was cancelled." } };
        }
        if (quoteRefusal) {
          normalizingTotal = 0;
          phase = "error";
          lastError = quoteRefusal;
          emit();
          return { ok: false, refusal: quoteRefusal };
        }

        const file = new File([request.bytes], request.file_name || "recording", {
          type: request.mime_type,
          ...(request.last_modified !== undefined ? { lastModified: request.last_modified } : {}),
        });
        const normalized = await normalizeAudioFile(file, {
          signal: ctrl.signal,
          onChunkNormalized: (_done, total) => {
            if (sessionId === currentSession && !ctrl.signal.aborted) {
              normalizingTotal = total;
              emit();
            }
          },
        });
        if (disposed || ctrl.signal.aborted || sessionId !== currentSession) {
          return { ok: false, refusal: { code: "cancelled", text: "Audio capture was cancelled." } };
        }
        heldAt = normalized.heldAt;
        elapsedSeconds = normalized.durationSeconds;
        normalizingTotal = normalized.chunks.length;
        segments = normalized.chunks.map((chunk) => ({
          chunk,
          requestId: `aud_${currentSession}_${chunk.index}`,
          status: "queued",
          text: "",
          errorCode: null,
        }));
        phase = "transcribing";
        emit();
        pumpSegments();
        await waitForDrain(currentSession);
        if (disposed || ctrl.signal.aborted || sessionId !== currentSession) {
          return { ok: false, refusal: { code: "cancelled", text: "Audio capture was cancelled." } };
        }
        phase = "ready";
        emit();
        return { ok: true, data: buildSnapshot() };
      } catch (err) {
        if (disposed || ctrl.signal.aborted || sessionId !== currentSession) {
          return { ok: false, refusal: { code: "cancelled", text: "Audio capture was cancelled." } };
        }
        normalizingTotal = 0;
        const code = err instanceof AudioNormalizeError ? err.code : "decode_failed";
        const refusal: ActionRefusal = { code, text: normalizeFailureMessage(err, t) };
        phase = "error";
        lastError = refusal;
        emit();
        return { ok: false, refusal };
      } finally {
        callSignal.removeEventListener("abort", onCallAbort);
      }
    }

    // request.action === "retry"
    if (callSignal.aborted) {
      return { ok: false, refusal: { code: "cancelled", text: "Audio capture was cancelled." } };
    }
    if (phase !== "ready" || !segments.some((s) => s.status === "failed")) {
      return { ok: false, refusal: { code: "conflict", text: "No failed audio segments to retry." } };
    }
    const currentSession = sessionId;
    if (!sessionAbort || sessionAbort.signal.aborted) {
      sessionAbort = new AbortController();
    }
    const retryAbort = sessionAbort;
    const onRetryAbort = () => {
      retryAbort.abort();
      const waiters = drainWaiters;
      drainWaiters = [];
      for (const wake of waiters) wake();
    };
    callSignal.addEventListener("abort", onRetryAbort, { once: true });
    lastError = null;
    for (const item of segments) {
      if (item.status === "failed") {
        item.status = "queued";
        item.errorCode = null;
      }
    }
    phase = "transcribing";
    emit();
    pumpSegments();
    try {
      await waitForDrain(currentSession);
    } finally {
      callSignal.removeEventListener("abort", onRetryAbort);
    }
    if (disposed || sessionId !== currentSession || retryAbort.signal.aborted) {
      return { ok: false, refusal: { code: "cancelled", text: "Audio capture was cancelled." } };
    }
    phase = "ready";
    emit();
    return { ok: true, data: buildSnapshot() };
  };

  return { dispatch, dispose };
}
