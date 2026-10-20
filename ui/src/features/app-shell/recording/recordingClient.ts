import { privateFetch, withPrivateOperation } from "../../../lib/privateSession";
import { sessionHeaders } from "../../../lib/sessionKey";

/** Typed client for install-scoped recording routes. Scope comes from the
 * route only; install and context ids are never shown. DTOs mirror the
 * backend contract exactly (snake_case, opaque ids, epoch-millisecond timestamps). */

export type RecordingState = "recording" | "finishing" | "transcribing" | "consolidating" | "ready" | "failed";
export type SegmentState = "queued" | "transcribing" | "confirmed" | "failed";
export type TitleOwner = "generated" | "manual";

export interface RecordingError { code: string; text: string; retryable: boolean }

export interface RecordingSummary {
  id: string;
  title: string | null;
  title_owner: TitleOwner | null;
  participants: string[];
  state: RecordingState;
  created_ms: number;
  updated_ms: number;
  duration_ms: number;
  segment_count: number;
  has_minutes: boolean;
}

export interface Segment {
  sequence: number;
  start_ms: number;
  end_ms: number;
  state: SegmentState;
  text: string | null;
}

export interface MinuteDecision { text: string; sources: number[] }
export interface MinuteFollowUp { text: string; owner: string | null; due: string | null; sources: number[] }
export interface Minutes { summary: string; decisions: MinuteDecision[]; followups: MinuteFollowUp[] }

export interface RecordingConsent {
  transcription_max_minor: number;
  consolidation_max_minor: number;
  currency: "USD";
}

export interface RecordingDetail extends RecordingSummary {
  consent: RecordingConsent;
  segments: Segment[];
  segments_next: number | null;
  minutes: Minutes | null;
  error: RecordingError | null;
}

export interface RecordingQuote {
  transcription_max_minor: number | null;
  consolidation_max_minor: number | null;
  currency: "USD";
  unavailable_reason: string | null;
}

export interface CreateRecordingBody {
  request_id: string;
  title?: string | null;
  participants?: string[];
  consent: RecordingConsent;
}


export interface SegmentUploadBody {
  sequence: number;
  digest: string;
  start_ms: number;
  end_ms: number;
  format: "wav16k-mono-pcm16";
  audio_base64: string;
}

export interface RecordingListPage { recordings: RecordingSummary[]; next_cursor: string | null }

/** Base path for one install + context. Built from the verified selected default; never displayed. */
export interface RecordingBase { installId: string; contextId: string }

export type ClientResult<T> = { ok: true; value: T } | { ok: false; error: RecordingError; status: number };

const enc = encodeURIComponent;
const root = (b: RecordingBase) => `/api/app-installations/${enc(b.installId)}/contexts/${enc(b.contextId)}/recordings`;

async function call<T>(method: string, url: string, body?: unknown, timeoutMs?: number): Promise<ClientResult<T>> {
  return withPrivateOperation(async (operation) => {
    const controller = new AbortController();
    const timer = timeoutMs === undefined ? null : window.setTimeout(() => controller.abort(), timeoutMs);
    try {
      const response = await privateFetch(url, {
        method,
        credentials: "same-origin",
        cache: "no-store",
        signal: controller.signal,
        headers: body === undefined ? sessionHeaders() : {
          "Content-Type": "application/json",
          "X-Cadence-Board": "1",
          ...sessionHeaders(),
        },
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      }, operation);
      const json: unknown = await response.json().catch(() => null);
      operation.assertCurrent();
      if (response.ok && json && typeof json === "object") return { ok: true, value: json as T };
      const err = (json as { error?: RecordingError } | null)?.error;
      const fallback: RecordingError = { code: "unavailable", text: "Cadence could not complete that request. Nothing was assumed saved.", retryable: true };
      return { ok: false, status: response.status, error: err && typeof err.text === "string" ? err : fallback };
    } catch (error) {
      operation.assertCurrent();
      if (controller.signal.aborted) return { ok: false, status: 0, error: { code: "uncertain_outcome", text: "The request timed out. Retry the same saved batch; no replacement paid call was started.", retryable: true } };
      return { ok: false, status: 0, error: { code: "network", text: "Cadence did not answer. The save may or may not have happened; check the recording before retrying.", retryable: true } };
    } finally {
      if (timer !== null) window.clearTimeout(timer);
    }
  });
}

export const recordingClient = {
  list(base: RecordingBase, cursor: string | null, limit = 20) {
    const q = new URLSearchParams({ limit: String(limit) });
    if (cursor) q.set("cursor", cursor);
    return call<RecordingListPage>("GET", `${root(base)}?${q}`);
  },
  quote(base: RecordingBase) {
    return call<RecordingQuote>("POST", `${root(base)}/quote`, {});
  },
  create(base: RecordingBase, body: CreateRecordingBody) {
    return call<{ recording: RecordingDetail }>("POST", root(base), body);
  },
  detail(base: RecordingBase, id: string, segmentsAfter = -1, segmentsLimit = 50) {
    const q = new URLSearchParams({ segments_after: String(segmentsAfter), segments_limit: String(segmentsLimit) });
    return call<{ recording: RecordingDetail }>("GET", `${root(base)}/${enc(id)}?${q}`);
  },
  /** Uploads may take ~60s (inline ASR). A timeout is uncertain, not failed:
   * callers must re-POST the same sequence and digest. */
  segment(base: RecordingBase, id: string, body: SegmentUploadBody) {
    return call<{ segment: Segment }>("POST", `${root(base)}/${enc(id)}/segments`, body, 75_000);
  },
  finish(base: RecordingBase, id: string, body: { request_id: string; last_sequence: number }) {
    return call<{ recording: RecordingDetail }>("POST", `${root(base)}/${enc(id)}/finish`, body);
  },
  retry(base: RecordingBase, id: string, requestId: string) {
    return call<{ recording: RecordingDetail }>("POST", `${root(base)}/${enc(id)}/retry`, { request_id: requestId });
  },
  rename(base: RecordingBase, id: string, requestId: string, title: string | null) {
    return call<{ recording: RecordingDetail }>("PATCH", `${root(base)}/${enc(id)}`, { request_id: requestId, title });
  },
};

/** Base64 of a WAV byte array, chunked so large segments do not blow the stack. */
export function base64Of(bytes: Uint8Array): string {
  let binary = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}

/** SHA-256 hex of the WAV bytes. The server re-checks it on upload. */
export async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", bytes as BufferSource);
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0")).join("");
}
