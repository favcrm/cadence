import { sessionHeaders } from "../../lib/sessionKey";
import { privateFetch, usingPrivateOperation } from "../../lib/privateSession";
import { ApiError, errorMessage } from "../../lib/apiError";

/** The shapes the host announces for a video job. */
export const VIDEO_ASPECTS = ["16:9", "9:16", "1:1", "auto"] as const;
export type VideoAspect = (typeof VIDEO_ASPECTS)[number];

/** The largest video the chat keeps; the host answers `too_large` above it. */
export const VIDEO_CAP_BYTES = 100 * 1024 * 1024;

/** A thread entry's `video_job` payload, validated. */
export interface VideoJob {
  jobId: string;
  title: string;
  aspect: VideoAspect;
  /** Seconds the job was asked for. */
  duration: number;
}

/** The job a stored `video_job` entry names; null when the payload is not
 *  exactly the contract's shape, so a malformed entry renders nothing. */
export function videoJob(payload: Record<string, unknown> | null | undefined): VideoJob | null {
  if (!payload) return null;
  const { job_id: jobId, title, aspect, duration } = payload;
  if (typeof jobId !== "string" || !/^med_[A-Za-z0-9_-]+$/.test(jobId)) return null;
  if (typeof title !== "string") return null;
  if (typeof aspect !== "string" || !(VIDEO_ASPECTS as readonly string[]).includes(aspect)) return null;
  if (typeof duration !== "number" || !Number.isFinite(duration) || duration <= 0) return null;
  return { jobId, title, aspect: aspect as VideoAspect, duration };
}

/** What the host says of a job. A finished one carries the stored video. */
export type VideoStatus =
  | { state: "running" | "failed" | "unclear" | "too_large" }
  | { state: "finished"; bytes: number; mime: string; duration: number };

/** `missing` is a 404, or a 401/403 (access to the conversation is gone): the
 *  job is not readable here, and asking again will not change that. */
export type VideoRead = { kind: "status"; status: VideoStatus } | { kind: "missing" };

/** Parse a status body; null when it is not the contract's shape. A
 *  `finished` without a playable video's size, type and length is malformed. */
export function parseVideoStatus(body: unknown): VideoStatus | null {
  if (typeof body !== "object" || body === null) return null;
  const { state, bytes, mime, duration } = body as Record<string, unknown>;
  if (state === "running" || state === "failed" || state === "unclear" || state === "too_large") return { state };
  if (state !== "finished") return null;
  if (typeof bytes !== "number" || !Number.isSafeInteger(bytes) || bytes < 1) return null;
  if (mime !== "video/mp4" && mime !== "video/webm") return null;
  if (typeof duration !== "number" || !Number.isFinite(duration) || duration <= 0) return null;
  return { state, bytes, mime, duration };
}

const jobUrl = (jobId: string, conversation: string, tail = "") =>
  `/api/video-jobs/${encodeURIComponent(jobId)}${tail}?conversation=${encodeURIComponent(conversation)}`;

/** Read one job's status through the board (operator session), for the
 *  conversation in view. Throws on a network error, a refusal or a body that
 *  is not the contract's shape: the caller keeps what it shows and retries. */
export function readVideoJob(jobId: string, conversation: string, signal?: AbortSignal): Promise<VideoRead> {
  return usingPrivateOperation(undefined, async (op) => {
    const resp = await privateFetch(jobUrl(jobId, conversation), { headers: sessionHeaders(), signal }, op);
    if (resp.status === 404 || resp.status === 401 || resp.status === 403) return { kind: "missing" } as const;
    if (!resp.ok) throw new Error(`${resp.status} ${resp.statusText}`);
    const body: unknown = await resp.json();
    op.assertCurrent();
    const status = parseVideoStatus(body);
    if (!status) throw new Error("Unreadable video status");
    return { kind: "status", status } as const;
  });
}

/** A finished job's bytes. Called only when the user plays or downloads. */
export function videoBytes(
  jobId: string,
  conversation: string,
  signal?: AbortSignal,
): Promise<{ bytes: ArrayBuffer; mime: string }> {
  return usingPrivateOperation(undefined, async (op) => {
    const resp = await privateFetch(jobUrl(jobId, conversation, "/bytes"), { headers: sessionHeaders(), signal }, op);
    if (!resp.ok) {
      const body = await resp.json().catch(() => null);
      throw new ApiError(errorMessage(body, `${resp.status} ${resp.statusText}`), resp.status);
    }
    const bytes = await resp.arrayBuffer();
    op.assertCurrent();
    return { bytes, mime: (resp.headers.get("Content-Type") ?? "").split(";")[0].trim() };
  });
}
