import { sessionHeaders } from "../../lib/sessionKey";
import { privateFetch, usingPrivateOperation } from "../../lib/privateSession";
import { artifactRef } from "./feed";
import type { ArtifactRef } from "./canvasStore";

/** The shapes the host announces for an image job. */
export const IMAGE_ASPECTS = ["1:1", "16:9", "9:16", "3:2", "2:3"] as const;
export type ImageAspect = (typeof IMAGE_ASPECTS)[number];

/** A thread entry's `image_job` payload, validated. */
export interface ImageJob {
  jobId: string;
  title: string;
  aspect: ImageAspect;
}

/** The job a stored `image_job` entry names; null when the payload is not
 *  exactly the contract's shape, so a malformed entry renders nothing. */
export function imageJob(payload: Record<string, unknown> | null | undefined): ImageJob | null {
  if (!payload) return null;
  const { job_id: jobId, title, aspect } = payload;
  if (typeof jobId !== "string" || !/^med_[A-Za-z0-9_-]+$/.test(jobId)) return null;
  if (typeof title !== "string") return null;
  if (typeof aspect !== "string" || !(IMAGE_ASPECTS as readonly string[]).includes(aspect)) return null;
  return { jobId, title, aspect: aspect as ImageAspect };
}

/** What the host says of a job. A finished one carries the stored image. */
export type JobStatus =
  | { state: "running" | "failed" | "unclear" }
  | { state: "finished"; artifact: ArtifactRef };

/** `missing` is a 404: the job is not there for this conversation. */
export type JobRead = { kind: "status"; status: JobStatus } | { kind: "missing" };

/** Parse a status body; null when it is not the contract's shape. A
 *  `finished` without a stored image of kind `image` is malformed, not done. */
export function parseJobStatus(body: unknown): JobStatus | null {
  if (typeof body !== "object" || body === null) return null;
  const { state, artifact } = body as Record<string, unknown>;
  if (state === "running" || state === "failed" || state === "unclear") return { state };
  if (state !== "finished") return null;
  if (typeof artifact !== "object" || artifact === null) return null;
  const ref = artifactRef(artifact as Record<string, unknown>);
  return ref && ref.kind === "image" ? { state, artifact: ref } : null;
}

/** Read one job's status through the board (operator session), for the
 *  conversation in view. Throws on a network error, a refusal or a body that
 *  is not the contract's shape: the caller keeps what it shows and retries. */
export function readImageJob(jobId: string, conversation: string, signal?: AbortSignal): Promise<JobRead> {
  return usingPrivateOperation(undefined, async (op) => {
    const url = `/api/image-jobs/${encodeURIComponent(jobId)}?conversation=${encodeURIComponent(conversation)}`;
    const resp = await privateFetch(url, { headers: sessionHeaders(), signal }, op);
    if (resp.status === 404) return { kind: "missing" } as const;
    if (!resp.ok) throw new Error(`${resp.status} ${resp.statusText}`);
    const body: unknown = await resp.json();
    op.assertCurrent();
    const status = parseJobStatus(body);
    if (!status) throw new Error("Unreadable image status");
    return { kind: "status", status } as const;
  });
}
