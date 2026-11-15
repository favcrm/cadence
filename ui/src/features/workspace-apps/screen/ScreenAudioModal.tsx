import { useEffect, useId, useMemo, useRef, useState } from "react";
import Button from "../../../ui/Button";
import { useTranslate } from "../../../lib/locale";
import { beginPrivateOperation, type PrivateOperation } from "../../../lib/privateSession";
import { sessionHeaders } from "../../../lib/sessionKey";
import { WorkspaceDialog } from "../WorkspaceDialog";
import {
  normalizeAudioFile,
  SCREEN_AUDIO_ACCEPT,
  startLiveMicrophoneCapture,
  type LiveMicrophoneCapture,
  type WavAudioChunk,
} from "./audioNormalize";
import {
  normalizeFailureMessage,
  randomAudioSessionId,
  readErrorCode,
  transcribeFailureMessage,
  unavailableMessage,
} from "./screenAudioSession";
import {
  AUDIO_TRANSCRIPT_CHARS_MAX,
  type ActionResult,
  type ScreenAudioCapture,
  type ScreenAudioMode,
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

interface SegmentItem {
  chunk: WavAudioChunk;
  requestId: string;
  status: "queued" | "transcribing" | "done" | "failed";
  text: string;
  errorCode: string | null;
}

function formatClock(totalSeconds: number): string {
  const clamped = Math.max(0, Math.floor(totalSeconds));
  const mins = Math.floor(clamped / 60);
  const secs = clamped % 60;
  return `${String(mins).padStart(2, "0")}:${String(secs).padStart(2, "0")}`;
}

export default function ScreenAudioModal({
  initialMode,
  actionToken,
  signal,
  onResolve,
}: {
  initialMode: ScreenAudioMode;
  actionToken: string;
  signal: AbortSignal;
  onResolve: (result: ActionResult) => void;
}) {
  const t = useTranslate();
  const fileInputId = useId();
  const [activeMode, setActiveMode] = useState<ScreenAudioMode>(initialMode);
  const [languageHint, setLanguageHint] = useState<"" | "en" | "zh">("");
  const [quoteState, setQuoteState] = useState<
    | { status: "checking" }
    | { status: "ready" }
    | { status: "unavailable"; message: string }
  >({ status: "checking" });
  const [quoteAttempt, setQuoteAttempt] = useState(0);

  // Recording state
  const [recordPhase, setRecordPhase] = useState<"idle" | "starting" | "recording" | "paused" | "stopped">("idle");
  const [elapsedMs, setElapsedMs] = useState(0);
  const [micLevel, setMicLevel] = useState(0);
  const [capturedDurationSec, setCapturedDurationSec] = useState(0);
  const [heldAt, setHeldAt] = useState<string>(() => new Date().toISOString());

  // Upload normalization state
  const [normalizing, setNormalizing] = useState<{ fileName: string; done: number; total: number } | null>(null);
  const [selectedFileName, setSelectedFileName] = useState<string | null>(null);
  const [dragActive, setDragActive] = useState(false);

  // Shared segments + transcript review state
  const [segments, setSegments] = useState<SegmentItem[]>([]);
  const [editedTranscript, setEditedTranscript] = useState<string | null>(null);
  const [bannerError, setBannerError] = useState<string | null>(null);

  const sessionNonceRef = useRef<string>(randomAudioSessionId());
  const recorderRef = useRef<LiveMicrophoneCapture | null>(null);
  const resolvedRef = useRef(false);
  const pumpingRef = useRef(false);
  const languageRef = useRef<"" | "en" | "zh">("");
  languageRef.current = languageHint;
  const segmentsRef = useRef<SegmentItem[]>([]);
  segmentsRef.current = segments;

  const onResolveRef = useRef(onResolve);
  onResolveRef.current = onResolve;

  const finishOnce = (result: ActionResult) => {
    if (resolvedRef.current) return;
    resolvedRef.current = true;
    const liveRecorder = recorderRef.current;
    recorderRef.current = null;
    if (liveRecorder) {
      void liveRecorder.abort();
    }
    onResolveRef.current(result);
  };

  // Abort if parent screen unmounts or private session pauses.
  useEffect(() => {
    if (signal.aborted) {
      finishOnce({ ok: false, refusal: { code: "cancelled", text: "Audio capture was cancelled." } });
      return;
    }
    const onAbort = () => {
      finishOnce({ ok: false, refusal: { code: "cancelled", text: "Audio capture was cancelled." } });
    };
    signal.addEventListener("abort", onAbort, { once: true });
    return () => {
      signal.removeEventListener("abort", onAbort);
      const liveRecorder = recorderRef.current;
      recorderRef.current = null;
      if (liveRecorder) {
        void liveRecorder.abort();
      }
    };
  }, [signal]);

  // Quote preflight
  useEffect(() => {
    let cancelled = false;
    let op: PrivateOperation | null = null;
    setQuoteState({ status: "checking" });
    setBannerError(null);
    try {
      op = beginPrivateOperation(signal);
    } catch {
      setQuoteState({
        status: "unavailable",
        message: t("Session verification is required before using audio transcription."),
      });
      return;
    }
    void (async () => {
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
        if (cancelled) return;
        if (!res.ok) {
          setQuoteState({
            status: "unavailable",
            message: unavailableMessage(readErrorCode(body), res.status, t),
          });
          return;
        }
        const quote = body as QuoteReply | null;
        if (!quote || typeof quote.available !== "boolean") {
          setQuoteState({
            status: "unavailable",
            message: t("Audio transcription is currently unavailable. Try again shortly."),
          });
          return;
        }
        if (!quote.available) {
          setQuoteState({
            status: "unavailable",
            message: unavailableMessage(quote.unavailable_reason, 200, t),
          });
          return;
        }
        setQuoteState({ status: "ready" });
      } catch {
        if (!cancelled) {
          setQuoteState({
            status: "unavailable",
            message: t("Could not reach the audio transcription service. Check your connection and retry."),
          });
        }
      } finally {
        op?.dispose();
      }
    })();
    return () => {
      cancelled = true;
      op?.dispose();
    };
  }, [actionToken, signal, quoteAttempt]);

  // Sequential segment transcription pump
  const pumpSegments = () => {
    if (pumpingRef.current || signal.aborted || resolvedRef.current) return;
    const next = segmentsRef.current.find((item) => item.status === "queued");
    if (!next) return;

    const activeSession = sessionNonceRef.current;
    pumpingRef.current = true;
    setSegments((prev) =>
      prev.map((item) =>
        item.requestId === next.requestId ? { ...item, status: "transcribing", errorCode: null } : item,
      ),
    );

    void (async () => {
      let op: PrivateOperation | null = null;
      try {
        op = beginPrivateOperation(signal);
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
            ...(languageRef.current ? { language: languageRef.current } : {}),
          }),
        });
        const body = (await res.json().catch(() => null)) as TranscribeReply | Record<string, unknown> | null;
        if (signal.aborted || resolvedRef.current || sessionNonceRef.current !== activeSession) return;
        if (!res.ok || !body || typeof (body as TranscribeReply).text !== "string") {
          const code = readErrorCode(body) ?? "failed";
          setSegments((prev) =>
            prev.map((item) =>
              item.requestId === next.requestId ? { ...item, status: "failed", errorCode: code } : item,
            ),
          );
          setBannerError(transcribeFailureMessage(code, t));
        } else {
          const text = (body as TranscribeReply).text.trim();
          setSegments((prev) =>
            prev.map((item) =>
              item.requestId === next.requestId
                ? {
                    ...item,
                    status: "done",
                    text,
                    errorCode: null,
                    chunk: { ...item.chunk, wavBytes: new Uint8Array(0), base64: "" },
                  }
                : item,
            ),
          );
        }
      } catch {
        if (!signal.aborted && !resolvedRef.current && sessionNonceRef.current === activeSession) {
          setSegments((prev) =>
            prev.map((item) =>
              item.requestId === next.requestId ? { ...item, status: "failed", errorCode: "network" } : item,
            ),
          );
          setBannerError(t("Network interrupted while transcribing. Retry failed segments."));
        }
      } finally {
        op?.dispose();
        pumpingRef.current = false;
        // Schedule next queued segment if any remain.
        queueMicrotask(pumpSegments);
      }
    })();
  };

  useEffect(() => {
    if (segments.some((s) => s.status === "queued")) {
      pumpSegments();
    }
  }, [segments]);

  const assembledTranscript = useMemo(
    () =>
      segments
        .filter((item) => item.status === "done" && item.text.length > 0)
        .map((item) => item.text)
        .join("\n")
        .slice(0, AUDIO_TRANSCRIPT_CHARS_MAX),
    [segments],
  );

  const effectiveTranscript = (editedTranscript ?? assembledTranscript).trim();
  const pendingCount = segments.filter((s) => s.status === "queued" || s.status === "transcribing").length;
  const failedCount = segments.filter((s) => s.status === "failed").length;
  const doneCount = segments.filter((s) => s.status === "done").length;
  const isRecordingActive = recordPhase === "starting" || recordPhase === "recording" || recordPhase === "paused";
  const isBusy = isRecordingActive || normalizing !== null || pendingCount > 0;
  const hasUnsavedCapture =
    isRecordingActive || segments.length > 0 || (editedTranscript !== null && editedTranscript.trim().length > 0);

  const requestClose = () => {
    if (hasUnsavedCapture) {
      const confirmed = window.confirm(
        t("Discard this audio capture and return without inserting a transcript?"),
      );
      if (!confirmed) return;
    }
    finishOnce({
      ok: false,
      refusal: { code: "cancelled", text: "Audio capture was cancelled." },
    });
  };

  const resetCaptureState = (nextMode: ScreenAudioMode) => {
    if (hasUnsavedCapture && !window.confirm(t("Discard the current audio capture and start over?"))) {
      return;
    }
    sessionNonceRef.current = randomAudioSessionId();
    setActiveMode(nextMode);
    setRecordPhase("idle");
    setElapsedMs(0);
    setMicLevel(0);
    setCapturedDurationSec(0);
    setNormalizing(null);
    setSelectedFileName(null);
    setSegments([]);
    setEditedTranscript(null);
    setBannerError(null);
  };

  const handleStartRecording = async () => {
    setBannerError(null);
    setEditedTranscript(null);
    setSegments([]);
    sessionNonceRef.current = randomAudioSessionId();
    const recordSession = sessionNonceRef.current;
    setRecordPhase("starting");
    try {
      const capture = await startLiveMicrophoneCapture({
        onLevel: (lvl) => setMicLevel(lvl),
        onElapsedMs: (ms) => setElapsedMs(ms),
        onSegment: (chunk) => {
          if (sessionNonceRef.current !== recordSession) return;
          const requestId = `aud_${recordSession}_${chunk.index}`;
          setSegments((prev) => [
            ...prev,
            { chunk, requestId, status: "queued", text: "", errorCode: null },
          ]);
        },
      });
      if (resolvedRef.current || signal.aborted || sessionNonceRef.current !== recordSession) {
        await capture.abort();
        return;
      }
      recorderRef.current = capture;
      setHeldAt(capture.startedAtIso);
      setRecordPhase("recording");
    } catch (error) {
      setRecordPhase("idle");
      const name = error instanceof DOMException ? error.name : "";
      if (name === "NotAllowedError" || name === "PermissionDeniedError") {
        setBannerError(t("Microphone access was blocked. Allow microphone permission in your browser settings and try again."));
      } else if (name === "NotFoundError" || name === "DevicesNotFoundError") {
        setBannerError(t("No microphone was found on this device."));
      } else {
        setBannerError(t("Could not start microphone recording in this browser."));
      }
    }
  };

  const handlePauseResume = () => {
    const live = recorderRef.current;
    if (!live) return;
    if (recordPhase === "recording") {
      live.pause();
      setRecordPhase("paused");
    } else if (recordPhase === "paused") {
      live.resume();
      setRecordPhase("recording");
    }
  };

  const handleStopRecording = async () => {
    const live = recorderRef.current;
    recorderRef.current = null;
    if (!live) return;
    const summary = await live.finish();
    setMicLevel(0);
    setCapturedDurationSec(Math.max(1, summary.durationSeconds));
    setRecordPhase("stopped");
    if (summary.totalFrames === 0) {
      setBannerError(t("No audio was captured. Start recording again when ready."));
    }
  };

  const handleFileChosen = async (file: File | null | undefined) => {
    if (!file || isBusy) return;
    if (hasUnsavedCapture && !window.confirm(t("Discard the current audio capture and start over?"))) {
      return;
    }
    setBannerError(null);
    setEditedTranscript(null);
    setSegments([]);
    sessionNonceRef.current = randomAudioSessionId();
    const uploadSession = sessionNonceRef.current;
    setSelectedFileName(file.name);
    setNormalizing({ fileName: file.name, done: 0, total: 1 });

    try {
      const normalized = await normalizeAudioFile(file, {
        signal,
        onChunkNormalized: (done, total) => {
          if (sessionNonceRef.current === uploadSession) {
            setNormalizing({ fileName: file.name, done, total });
          }
        },
      });
      if (signal.aborted || resolvedRef.current || sessionNonceRef.current !== uploadSession) return;
      setNormalizing(null);
      setHeldAt(normalized.heldAt);
      setCapturedDurationSec(normalized.durationSeconds);
      const queued: SegmentItem[] = normalized.chunks.map((chunk) => ({
        chunk,
        requestId: `aud_${uploadSession}_${chunk.index}`,
        status: "queued",
        text: "",
        errorCode: null,
      }));
      setSegments(queued);
    } catch (error) {
      if (signal.aborted || resolvedRef.current || sessionNonceRef.current !== uploadSession) return;
      setNormalizing(null);
      setBannerError(normalizeFailureMessage(error, t));
    }
  };

  const handleRetryFailed = () => {
    setBannerError(null);
    setSegments((prev) =>
      prev.map((item) =>
        item.status === "failed" ? { ...item, status: "queued", errorCode: null } : item,
      ),
    );
  };

  const handleUseTranscript = () => {
    if (!effectiveTranscript || isBusy) return;
    if (
      failedCount > 0 &&
      !window.confirm(t("Some audio segments failed to transcribe. Use the partial transcript anyway?"))
    ) {
      return;
    }
    const totalChunkMs = segments.reduce((acc, s) => acc + s.chunk.durationMs, 0);
    const durationSeconds =
      capturedDurationSec > 0
        ? capturedDurationSec
        : Math.max(1, Math.round(totalChunkMs / 1000));
    let boundedTranscript = effectiveTranscript.slice(0, AUDIO_TRANSCRIPT_CHARS_MAX);
    const maxUtf8Bytes = 240 * 1024;
    if (new TextEncoder().encode(boundedTranscript).length > maxUtf8Bytes) {
      let lo = 0;
      let hi = boundedTranscript.length;
      while (lo < hi) {
        const mid = Math.ceil((lo + hi) / 2);
        if (new TextEncoder().encode(boundedTranscript.slice(0, mid)).length <= maxUtf8Bytes) {
          lo = mid;
        } else {
          hi = mid - 1;
        }
      }
      boundedTranscript = boundedTranscript.slice(0, lo);
    }
    const payload: ScreenAudioCapture = {
      transcript: boundedTranscript,
      duration_seconds: durationSeconds,
      held_at: heldAt,
      source: activeMode === "record" ? "recording" : "upload",
    };
    finishOnce({ ok: true, data: payload });
  };

  const dialogTitle =
    activeMode === "record" ? t("Record audio") : t("Upload audio file");

  return (
    <WorkspaceDialog
      title={dialogTitle}
      className="wa-confirm wa-audio-dialog"
      onClose={requestClose}
    >
      <div className="wa-stack wa-audio-body" data-screen-audio-modal={activeMode}>
        <div className="wa-audio-tabs" role="group" aria-label={t("Audio capture mode")}>
          <button
            type="button"
            aria-pressed={activeMode === "record"}
            className={activeMode === "record" ? "wa-audio-tab is-active" : "wa-audio-tab"}
            disabled={isBusy}
            onClick={() => {
              if (activeMode !== "record") resetCaptureState("record");
            }}
          >
            {t("Record audio")}
          </button>
          <button
            type="button"
            aria-pressed={activeMode === "upload"}
            className={activeMode === "upload" ? "wa-audio-tab is-active" : "wa-audio-tab"}
            disabled={isBusy}
            onClick={() => {
              if (activeMode !== "upload") resetCaptureState("upload");
            }}
          >
            {t("Upload audio file")}
          </button>
        </div>

        {quoteState.status === "checking" && (
          <div className="wa-audio-status" role="status">
            {t("Checking audio transcription availability…")}
          </div>
        )}

        {quoteState.status === "unavailable" && (
          <div className="wa-audio-alert" role="alert">
            <p>{quoteState.message}</p>
            <div className="wa-audio-actions-inline">
              <Button size="sm" onClick={() => setQuoteAttempt((n) => n + 1)}>
                {t("Retry")}
              </Button>
            </div>
          </div>
        )}

        {quoteState.status === "ready" && (
          <>
            {bannerError && (
              <div className="wa-audio-alert" role="alert">
                <p>{bannerError}</p>
                {failedCount > 0 && !isRecordingActive && (
                  <div className="wa-audio-actions-inline">
                    <Button size="sm" variant="primary" onClick={handleRetryFailed}>
                      {t("Retry failed segments")}
                    </Button>
                  </div>
                )}
              </div>
            )}

            {activeMode === "record" ? (
              <div className="wa-audio-panel">
                {recordPhase === "idle" && (
                  <div className="wa-audio-start">
                    <p className="wa-audio-hint">
                      {t("Start speaking immediately — audio stays in your browser and transcribes in 10-second segments.")}
                    </p>
                    <Button
                      variant="primary"
                      onClick={() => {
                        void handleStartRecording();
                      }}
                    >
                      {t("Start recording")}
                    </Button>
                  </div>
                )}

                {recordPhase === "starting" && (
                  <div className="wa-audio-status" role="status">
                    {t("Requesting microphone access…")}
                  </div>
                )}

                {(recordPhase === "recording" || recordPhase === "paused") && (
                  <div className="wa-audio-live">
                    <div className="wa-audio-meter-row">
                      <span
                        className={
                          recordPhase === "paused"
                            ? "wa-audio-dot is-paused"
                            : "wa-audio-dot is-recording"
                        }
                        aria-hidden="true"
                      />
                      <span className="num wa-audio-clock" aria-live="off">
                        {formatClock(elapsedMs / 1000)}
                      </span>
                      <span className="wa-audio-badge">
                        {recordPhase === "paused" ? t("Paused") : t("Recording")}
                      </span>
                    </div>
                    <div
                      className="wa-audio-meter"
                      role="progressbar"
                      aria-label={t("Microphone input level")}
                      aria-valuemin={0}
                      aria-valuemax={100}
                      aria-valuenow={Math.round(micLevel * 100)}
                    >
                      <div
                        className="wa-audio-meter-fill"
                        style={{ width: `${Math.round(micLevel * 100)}%` }}
                      />
                    </div>
                    <div className="wa-audio-controls">
                      <Button onClick={handlePauseResume}>
                        {recordPhase === "paused" ? t("Resume") : t("Pause")}
                      </Button>
                      <Button
                        variant="primary"
                        onClick={() => {
                          void handleStopRecording();
                        }}
                      >
                        {t("Finish recording")}
                      </Button>
                    </div>
                  </div>
                )}

                {recordPhase === "stopped" && (
                  <div className="wa-audio-stopped-bar">
                    <span>
                      {t("Recorded duration")}: <strong className="num">{formatClock(capturedDurationSec)}</strong>
                    </span>
                    <Button
                      size="sm"
                      disabled={pendingCount > 0}
                      onClick={() => resetCaptureState("record")}
                    >
                      {t("Record again")}
                    </Button>
                  </div>
                )}
              </div>
            ) : (
              <div className="wa-audio-panel">
                <label
                  htmlFor={fileInputId}
                  className={dragActive ? "wa-audio-dropzone is-drag" : "wa-audio-dropzone"}
                  onDragOver={(event) => {
                    event.preventDefault();
                    if (!isBusy) setDragActive(true);
                  }}
                  onDragLeave={() => setDragActive(false)}
                  onDrop={(event) => {
                    event.preventDefault();
                    setDragActive(false);
                    if (isBusy) return;
                    const file = event.dataTransfer.files?.[0];
                    void handleFileChosen(file);
                  }}
                >
                  <input
                    id={fileInputId}
                    type="file"
                    accept={SCREEN_AUDIO_ACCEPT}
                    disabled={isBusy}
                    className="sr-only"
                    onChange={(event) => {
                      const file = event.currentTarget.files?.[0];
                      event.currentTarget.value = "";
                      void handleFileChosen(file);
                    }}
                  />
                  <strong>
                    {selectedFileName
                      ? selectedFileName
                      : t("Choose or drop an audio recording")}
                  </strong>
                  <span className="wa-audio-formats">
                    {t(
                      "Supports iPhone Voice Memos (.m4a), WhatsApp (.opus / .ogg), Android (.m4a, .aac, .3gp, .amr), MP3, WAV, WebM, and FLAC up to 50 MB.",
                    )}
                  </span>
                  <span className="btn btn-sm" aria-hidden="true">
                    {selectedFileName ? t("Choose another file") : t("Select audio file")}
                  </span>
                </label>
              </div>
            )}

            {normalizing && (
              <div className="wa-audio-status" role="status">
                {t("Preparing audio ({done}/{total})…")
                  .replace("{done}", () => String(normalizing.done))
                  .replace("{total}", () => String(normalizing.total))}
              </div>
            )}

            {segments.length > 0 && (
              <div className="wa-audio-progress" role="status" aria-live="polite">
                {pendingCount > 0
                  ? t("Transcribing segment {done} of {total}…")
                      .replace("{done}", () => String(Math.min(doneCount + 1, segments.length)))
                      .replace("{total}", () => String(segments.length))
                  : failedCount > 0
                    ? t("{failed} segment(s) failed — {done} completed.")
                        .replace("{failed}", () => String(failedCount))
                        .replace("{done}", () => String(doneCount))
                    : t("Transcription complete ({total} segment(s)).").replace(
                        "{total}",
                        () => String(segments.length),
                      )}
              </div>
            )}

            {(segments.length > 0 || editedTranscript !== null) && (
              <label className="wa-stack wa-audio-transcript-field">
                <span className="wa-audio-label">{t("Transcript preview")}</span>
                <textarea
                  className="wa-input wa-audio-textarea"
                  rows={6}
                  value={editedTranscript ?? assembledTranscript}
                  readOnly={isBusy}
                  placeholder={
                    isBusy
                      ? t("Transcribed speech will appear here…")
                      : t("No speech detected in this audio.")
                  }
                  onChange={(event) => setEditedTranscript(event.currentTarget.value)}
                />
              </label>
            )}

            <details className="wa-audio-lang-details">
              <summary>{t("Speech language hint (optional)")}</summary>
              <div className="wa-audio-lang-row">
                <label htmlFor={`${fileInputId}-lang`} className="wa-audio-hint">
                  {t(
                    "Auto-detect handles Cantonese, English, and bilingual code-switching automatically.",
                  )}
                </label>
                <select
                  id={`${fileInputId}-lang`}
                  className="wa-input"
                  value={languageHint}
                  disabled={isBusy}
                  onChange={(event) =>
                    setLanguageHint(event.currentTarget.value as "" | "en" | "zh")
                  }
                >
                  <option value="">
                    {t("Auto-detect (Cantonese / English / Multilingual)")}
                  </option>
                  <option value="en">{t("English (en)")}</option>
                  <option value="zh">{t("Chinese (zh)")}</option>
                </select>
              </div>
            </details>
          </>
        )}

        <div className="wa-audio-footer">
          <Button onClick={requestClose}>{t("Cancel")}</Button>
          <Button
            variant="primary"
            disabled={quoteState.status !== "ready" || isBusy || effectiveTranscript.length === 0}
            onClick={handleUseTranscript}
          >
            {t("Use transcript")}
          </Button>
        </div>
      </div>
    </WorkspaceDialog>
  );
}
