import { useCallback, useEffect, useRef, useState } from "react";
import { startCapture, TARGET_RATE, type CaptureHandle, type CaptureTimeline } from "./wavCapture";
import { base64Of, recordingClient, sha256Hex, type RecordingBase, type RecordingConsent, type RecordingDetail, type SegmentUploadBody } from "./recordingClient";
import { navigate, registerNavigationGuard } from "../../../lib/useLocation";

export type SessionPhase = "ready" | "starting" | "recording" | "paused" | "saving" | "finishing" | "transcribing" | "consolidating" | "failed" | "error";
export interface SessionState {
  phase: SessionPhase;
  recordingId: string | null;
  detail: RecordingDetail | null;
  elapsedMs: number;
  level: number;
  pendingSegments: number;
  error: string | null;
}
const INITIAL: SessionState = { phase: "ready", recordingId: null, detail: null, elapsedMs: 0, level: 0, pendingSegments: 0, error: null };
interface Pending { wav: Uint8Array; sequence: number; startMs: number; endMs: number; body?: SegmentUploadBody }
const delay = () => new Promise<void>((resolve) => { window.setTimeout(resolve, 1500); });

/** The shell keys this owner by its verified installation/context receipt. */
export function useRecordingSession(base: RecordingBase, consent: RecordingConsent | null) {
  const [state, setState] = useState<SessionState>(INITIAL);
  const live = useRef(true);
  const busy = useRef(false);
  const capture = useRef<CaptureHandle | null>(null);
  const stopping = useRef<Promise<void> | null>(null);
  const discarding = useRef(false);
  const timeline = useRef<CaptureTimeline>({ sequence: 0, frames: 0 });
  const recordingId = useRef<string | null>(null);
  const queue = useRef<Pending[]>([]);
  const draining = useRef<Promise<void> | null>(null);
  const [finishKey] = useState(() => crypto.randomUUID());
  const retryKey = useRef<string | null>(null);
  const creation = useRef<Parameters<typeof recordingClient.create>[1] | null>(null);
  const captureFailure = useRef(false);
  const patch = useCallback((next: Partial<SessionState>) => { if (live.current) setState((old) => ({ ...old, ...next })); }, []);
  const stopCapture = useCallback(async () => {
    if (stopping.current) return stopping.current;
    const owner = capture.current;
    capture.current = null;
    if (owner) {
      stopping.current = owner.stop().finally(() => { stopping.current = null; });
      await stopping.current;
    }
    patch({ level: 0 });
  }, [patch]);
  const fail = useCallback((error: unknown) => {
    captureFailure.current = true;
    patch({ phase: "error", error: error instanceof Error ? error.message : "The recording needs attention. Saved transcript remains available." });
    void stopCapture().catch((stopError: unknown) => patch({ error: stopError instanceof Error ? stopError.message : "The last audio block could not be confirmed." }));
  }, [patch, stopCapture]);

  const drain = useCallback((): Promise<void> => {
    if (draining.current) return draining.current;
    const run = (async () => {
      while (!discarding.current && queue.current.length && recordingId.current) {
        const next = queue.current[0];
        next.body ??= { sequence: next.sequence, digest: await sha256Hex(next.wav), start_ms: next.startMs, end_ms: next.endMs, format: "wav16k-mono-pcm16", audio_base64: base64Of(next.wav) };
        if (discarding.current) return;
        const response = await recordingClient.segment(base, recordingId.current, next.body);
        if (!response.ok) throw new Error(response.error.text);
        let segment = response.value.segment;
        while (segment.state === "queued" || segment.state === "transcribing") {
          await delay();
          if (discarding.current) return;
          const current = await recordingClient.detail(base, recordingId.current, next.sequence - 1, 1);
          if (!current.ok) throw new Error(current.error.text);
          const stored = current.value.recording.segments.find((item) => item.sequence === next.sequence);
          if (live.current) setState((old) => ({ ...old, detail: { ...current.value.recording, segments: old.detail?.segments ?? current.value.recording.segments } }));
          if (!stored) throw new Error("The pending audio batch could not be verified. Retry it before leaving.");
          segment = stored;
        }
        if (segment.state !== "confirmed") throw new Error("This audio batch was not transcribed. Retry the saved batch; no transcript was assumed.");
        queue.current.shift();
        if (live.current) setState((old) => ({ ...old, pendingSegments: queue.current.length, detail: old.detail ? {
          ...old.detail,
          duration_ms: Math.max(old.detail.duration_ms, segment.end_ms),
          segment_count: Math.max(old.detail.segment_count, segment.sequence + 1),
          segments: [...old.detail.segments.filter((item) => item.sequence !== segment.sequence), segment].sort((a, b) => a.sequence - b.sequence),
        } : null }));
      }
    })();
    draining.current = run.finally(() => { draining.current = null; });
    return draining.current;
  }, [base, patch]);

  useEffect(() => {
    live.current = true;
    const beforeUnload = (event: BeforeUnloadEvent) => {
      if (capture.current || stopping.current || queue.current.length) { event.preventDefault(); event.returnValue = ""; }
    };
    window.addEventListener("beforeunload", beforeUnload);
    return () => {
      live.current = false;
      window.removeEventListener("beforeunload", beforeUnload);
      // Stop the microphone, then persist already-authorized pending audio;
      // leaving never finalizes minutes or leaves background capture running.
      void stopCapture().then(drain).catch(() => { /* The server remains unfinished, never falsely complete. */ });
    };
  }, [drain, stopCapture]);

  const create = useCallback(async () => {
    if (!creation.current) throw new Error("No recording was requested.");
    const response = await recordingClient.create(base, creation.current);
    if (!response.ok) throw new Error(response.error.text);
    recordingId.current = response.value.recording.id;
    patch({ recordingId: recordingId.current, detail: response.value.recording });
  }, [base, patch]);

  const start = useCallback(async (title: string, participants: string) => {
    if (busy.current || creation.current || !consent) return;
    busy.current = true;
    patch({ phase: "starting", error: null });
    try {
      captureFailure.current = false;
      const owner = await startCapture(timeline.current, {
        onLevel: (level, frames) => patch({ level, elapsedMs: Math.round(frames * 1000 / TARGET_RATE) }),
        onFailure: (message) => { captureFailure.current = true; fail(new Error(message)); },
        queued: () => queue.current.length,
        onSegment: (wav, sequence, startMs, endMs) => {
          if (discarding.current) return;
          queue.current.push({ wav, sequence, startMs, endMs });
          patch({ pendingSegments: queue.current.length });
          if (recordingId.current) void drain().catch(fail);
        },
      });
      capture.current = owner;
      if (!live.current) { await stopCapture(); return; }
      // Permission/device failure creates no orphan meeting. Audio collected
      // while create is pending is bounded and retains its exact retry identity.
      creation.current = { request_id: crypto.randomUUID(), title: title.trim() || null,
        participants: participants.split(",").map((name) => name.trim()).filter(Boolean), consent };
      await create();
      if (!live.current) { await stopCapture(); await drain(); return; }
      if (!captureFailure.current) patch({ phase: capture.current ? "recording" : "paused" });
      void drain().catch(fail);
    } catch (error) { fail(error); }
    finally { busy.current = false; }
  }, [consent, create, drain, fail, patch, stopCapture]);

  const pause = useCallback(async () => {
    if (busy.current || !capture.current) return;
    busy.current = true;
    try { await stopCapture(); if (!captureFailure.current) patch({ phase: "paused" }); }
    catch (error) { fail(error); }
    finally { busy.current = false; }
  }, [fail, patch, stopCapture]);

  const poll = useCallback(async () => {
    while (live.current && recordingId.current) {
      const response = await recordingClient.detail(base, recordingId.current);
      if (!response.ok) throw new Error(response.error.text);
      const detail = response.value.recording;
      patch({ detail, phase: detail.state, error: detail.error?.text ?? null });
      if (detail.state === "ready" || detail.state === "failed") return;
      await delay();
    }
  }, [base, patch]);

  const finish = useCallback(async () => {
    if (discarding.current) return;
    if (!recordingId.current) throw new Error("The meeting has not been saved yet. Retry the request first.");
    await drain();
    if (discarding.current) return;
    const response = await recordingClient.finish(base, recordingId.current, { request_id: finishKey, last_sequence: timeline.current.sequence - 1 });
    if (!response.ok) throw new Error(response.error.text);
    patch({ detail: response.value.recording });
    await poll();
  }, [base, drain, finishKey, patch, poll]);

  const stopAndWrite = useCallback(async () => {
    if (busy.current) return;
    busy.current = true;
    patch({ phase: "finishing", error: null });
    try { await stopCapture(); await finish(); }
    catch (error) { fail(error); }
    finally { busy.current = false; }
  }, [fail, finish, patch, stopCapture]);

  const resume = useCallback(async () => {
    if (busy.current || capture.current || !recordingId.current) return;
    busy.current = true;
    try {
      capture.current = await startCapture(timeline.current, {
        onLevel: (level, frames) => patch({ level, elapsedMs: Math.round(frames * 1000 / TARGET_RATE) }),
        onFailure: (message) => { captureFailure.current = true; fail(new Error(message)); },
        queued: () => queue.current.length,
        onSegment: (wav, sequence, startMs, endMs) => {
          if (discarding.current) return;
          queue.current.push({ wav, sequence, startMs, endMs });
          patch({ pendingSegments: queue.current.length });
          void drain().catch(fail);
        },
      });
      if (!live.current) { await stopCapture(); return; }
      patch({ phase: "recording", error: null });
    } catch (error) { fail(error); }
    finally { busy.current = false; }
  }, [drain, fail, patch, stopCapture]);

  const retry = useCallback(async () => {
    if (busy.current) return;
    busy.current = true;
    patch({ phase: "finishing", error: null });
    try {
      await stopCapture();
      if (discarding.current) return;
      if (!recordingId.current) await create();
      if (discarding.current) return;
      retryKey.current ??= crypto.randomUUID();
      const response = await recordingClient.retry(base, recordingId.current!, retryKey.current);
      if (!response.ok) throw new Error(response.error.text);
      retryKey.current = null;
      await finish();
    } catch (error) { fail(error); }
    finally { busy.current = false; }
  }, [base, create, fail, finish, patch, stopCapture]);

  const leave = useCallback(async (): Promise<boolean> => {
    if (busy.current) return !capture.current && !stopping.current && queue.current.length === 0;
    busy.current = true;
    patch({ phase: "saving", error: null });
    try {
      await stopCapture();
      await drain();
      if (queue.current.length) throw new Error("This audio has not been saved. Retry the recording or explicitly discard the unsaved audio before leaving.");
      return true;
    }
    catch (error) { fail(error); return false; }
    finally { busy.current = false; }
  }, [drain, fail, patch, stopCapture]);
  const discard = useCallback(async () => {
    discarding.current = true;
    queue.current = [];
    await stopCapture().catch(() => {});
    patch({ pendingSegments: 0 });
  }, [patch, stopCapture]);
  const leavePrompt = useRef(false);
  useEffect(() => {
    const here = new URL(location.href);
    return registerNavigationGuard((href) => {
      if (discarding.current) return true;
      const next = new URL(href, here);
      if (next.pathname === here.pathname && next.searchParams.get("ctx") === here.searchParams.get("ctx")) return true;
      if (!capture.current && !stopping.current && queue.current.length === 0) return true;
      if (leavePrompt.current) return false;
      leavePrompt.current = true;
      // Flush the worklet before a dialog can block delivery of its final audio.
      void stopCapture().then(() => {
        if (!live.current || discarding.current) return;
        setState((old) => ({ ...old, phase: old.error ? "error" : "paused", level: 0 }));
        if (window.confirm("Leave and discard audio not yet saved? Submitted work may finish; confirmed transcript is kept. Cancel keeps the recording paused here.")) {
          discarding.current = true;
          queue.current = [];
          navigate(href);
        } else if (!captureFailure.current) {
          void drain().catch(fail);
        }
      }).catch(fail).finally(() => { leavePrompt.current = false; });
      return false;
    });
  }, [drain, fail, patch, stopCapture]);
  return { state, start, pause, resume, stopAndWrite, retry, leave, discard, canStart: creation.current === null, canRetry: creation.current !== null };
}
