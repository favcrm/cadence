import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslate } from "../../lib/locale";
import type { ThreadEntry } from "../home/thread";
import { downloadName, imageDataUrl, saveBytes } from "./ArtifactView";
import { artifactBytes } from "./artifactClient";
import { ApiError } from "../../lib/apiError";
import { canvasSnapshot, openArtifact, resetCanvas, useCanvas, type ArtifactRef } from "./canvasStore";
import { useImageConversation } from "./imageConversation";
import { imageJob, readImageJob, type JobStatus } from "./imageJobClient";
import ImageOverlay from "./ImageOverlay";

/** How often a running job is read while the page is visible. */
export const POLL_MS = 5000;
/** The longest wait between retries after a failed read. */
export const BACKOFF_MAX_MS = 60_000;
/** PROVISIONAL: the point at which "Taking longer than usual" appears. Real
 *  job durations are not known yet (the accepted design mocks about 3
 *  minutes); set it from measured durations once there are some. */
export const LONG_WAIT_MS = 3 * 60 * 1000;
/** A job still `running` after this long is no longer being swept by the host,
 *  so it is shown as unconfirmed instead of "Still creating" forever. */
export const GIVE_UP_MS = 24 * 60 * 60 * 1000;
/** How far outside the viewport an image starts loading its bytes. */
const NEAR_MARGIN = "300px";

type Phase = "pending" | "running" | "finished" | "failed" | "unclear" | "unavailable";
interface JobView {
  phase: Phase;
  artifact?: ArtifactRef;
}

const TERMINAL: ReadonlySet<Phase> = new Set(["finished", "failed", "unclear", "unavailable"]);

/** What a status reads as. A job still running past `GIVE_UP_MS` since its
 *  entry was stored reads as unconfirmed: the host has stopped looking. */
const viewOf = (status: JobStatus, created: string | null | undefined): JobView => {
  if (status.state === "finished") return { phase: "finished", artifact: status.artifact };
  const start = typeof created === "string" ? Date.parse(created) : NaN;
  if (status.state === "running" && !Number.isNaN(start) && Date.now() - start >= GIVE_UP_MS) return { phase: "unclear" };
  return { phase: status.state };
};

/**
 * What the host says of one image job. Nothing is invented: until a read
 * succeeds the phase is `pending`, and a network error, a refusal or an
 * unreadable body keeps it there (retrying with backoff) rather than
 * showing a result. Polls only while the page is visible and the job runs.
 */
function useImageJob(jobId: string, conversation: string | null, near: boolean, created: string | null | undefined) {
  const [view, setView] = useState<JobView>({ phase: "pending" });
  const [checking, setChecking] = useState(false);
  // The phase announced to assistive tech: only a result that arrives while
  // this block was watching a running job, never an old image on page load.
  const [announced, setAnnounced] = useState<Phase | null>(null);
  const sawRunning = useRef(false);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const apply = useCallback((next: JobView) => {
    if (!mounted.current) return;
    if (next.phase === "running") sawRunning.current = true;
    if (TERMINAL.has(next.phase) && sawRunning.current) {
      sawRunning.current = false;
      setAnnounced(next.phase);
    }
    setView((cur) => (cur.phase === next.phase && cur.artifact?.id === next.artifact?.id ? cur : next));
  }, []);

  const live = view.phase === "pending" || view.phase === "running";
  useEffect(() => {
    // The first read waits until the box is near the viewport, so a long
    // thread does not read every job it holds. A job that has been near keeps
    // being read while the page is visible.
    if (!live || conversation === null || !near) return;
    let stopped = false;
    let inflight = false;
    let failures = 0;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const ctl = new AbortController();
    const visible = () => document.visibilityState !== "hidden";
    const clear = () => {
      if (timer !== undefined) clearTimeout(timer);
      timer = undefined;
    };
    const later = (ms: number) => {
      clear();
      if (!stopped && visible()) timer = setTimeout(() => void poll(), ms);
    };
    const poll = async () => {
      timer = undefined;
      if (stopped || inflight || !visible()) return;
      inflight = true;
      try {
        const read = await readImageJob(jobId, conversation, ctl.signal);
        if (stopped) return;
        failures = 0;
        if (read.kind === "missing") apply({ phase: "unavailable" });
        else {
          const next = viewOf(read.status, created);
          apply(next);
          if (next.phase === "running") later(POLL_MS);
        }
      } catch {
        if (stopped) return;
        failures += 1;
        later(Math.min(POLL_MS * 2 ** (failures - 1), BACKOFF_MAX_MS));
      } finally {
        inflight = false;
      }
    };
    // A hidden page stops asking; coming back reads at once.
    const onVisibility = () => {
      if (!visible()) clear();
      else if (timer === undefined && !inflight) void poll();
    };
    document.addEventListener("visibilitychange", onVisibility);
    void poll();
    return () => {
      stopped = true;
      clear();
      ctl.abort();
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [live, jobId, conversation, near, created, apply]);

  /** Check again: one read. A failed read leaves the card as it was. */
  const recheck = useCallback(async () => {
    if (conversation === null || checking) return;
    setChecking(true);
    try {
      const read = await readImageJob(jobId, conversation);
      if (read.kind === "missing") apply({ phase: "unavailable" });
      else apply(viewOf(read.status, created));
    } catch {
      // Still unconfirmed: the card stays, and the button comes back.
    } finally {
      if (mounted.current) setChecking(false);
    }
  }, [apply, checking, conversation, created, jobId]);

  return { view, announced, checking, recheck };
}

/** Seconds since the entry was stored, or null when it carries no usable time. */
export function useElapsed(created: string | null | undefined, active: boolean): number | null {
  const start = typeof created === "string" ? Date.parse(created) : NaN;
  const valid = !Number.isNaN(start);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active || !valid) return;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const tick = () => {
      setNow(Date.now());
      timer = setTimeout(tick, 1000);
    };
    tick();
    return () => clearTimeout(timer);
  }, [active, valid]);
  return valid ? Math.max(0, now - start) : null;
}

/** The bytes a `data:` URL carries. */
function dataUrlBytes(url: string): ArrayBuffer {
  const bin = atob(url.slice(url.indexOf(",") + 1));
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out.buffer;
}

export const clock = (ms: number): string => {
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
};

type Bytes = { status: "idle" } | { status: "ready"; src: string; mime: string; size: number } | { status: "failed" };

/** The picture's bytes, fetched once its box is near the viewport. The board
 *  CSP allows `data:` images and not `blob:` ones, so they become a data URL. */
function useImageBytes(artifact: ArtifactRef | undefined, conversation: string | null, near: boolean): Bytes {
  const [state, setState] = useState<Bytes>({ status: "idle" });
  const [tries, setTries] = useState(0);
  const id = artifact?.id;
  const version = artifact?.version;
  useEffect(() => {
    if (id === undefined || version === undefined || conversation === null || !near) return;
    const abort = new AbortController();
    let retry: ReturnType<typeof setTimeout> | undefined;
    artifactBytes(id, version, conversation, abort.signal)
      .then((data) => {
        if (abort.signal.aborted) return;
        // No picture without bytes that are one.
        if (!data.mime.startsWith("image/")) setState({ status: "failed" });
        else setState({ status: "ready", src: imageDataUrl(data), mime: data.mime, size: data.bytes.byteLength });
      })
      .catch((e: unknown) => {
        if (abort.signal.aborted) return;
        // Only a refusal or a missing version is final; a temporary fault asks again.
        if (e instanceof ApiError && [401, 403, 404].includes(e.status)) setState({ status: "failed" });
        else retry = setTimeout(() => setTries((n) => n + 1), Math.min(POLL_MS * 2 ** tries, BACKOFF_MAX_MS));
      });
    return () => {
      abort.abort();
      if (retry !== undefined) clearTimeout(retry);
    };
  }, [id, version, conversation, near, tries]);
  return state;
}

/** True once `ref` has come within `NEAR_MARGIN` of the viewport. */
export function useNear(ref: React.RefObject<HTMLElement | null>): boolean {
  const [near, setNear] = useState(false);
  useEffect(() => {
    if (near) return;
    const el = ref.current;
    if (!el || typeof IntersectionObserver === "undefined") {
      setNear(true);
      return;
    }
    const observer = new IntersectionObserver(
      (hits) => {
        if (hits.some((h) => h.isIntersecting)) setNear(true);
      },
      { rootMargin: NEAR_MARGIN },
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, [near, ref]);
  return near;
}

export function Waiting({ label, sub, elapsed }: { label?: string; sub?: string; elapsed?: number | null }) {
  return (
    <div className="imgwait">
      <span className="imgdots" aria-hidden>
        <i />
        <i />
        <i />
      </span>
      {label ? <b>{label}</b> : null}
      {label && elapsed != null ? <span className="imgclock num" aria-hidden>{clock(elapsed)}</span> : null}
      {sub ? <small>{sub}</small> : null}
    </div>
  );
}

/**
 * A generated image in the thread (CAD-2069): one box at the image's final
 * shape from first paint, so the picture replaces the placeholder in place.
 * The thread entry carries only a job id, title and shape; the host says how
 * the job stands, and the bytes come through the artifact client. Failed and
 * unclear jobs are short cards. There is deliberately no Stop, Open or
 * Make another: the job runs regardless of the turn, and a change is asked
 * for in the chat. Nothing here mentions cost.
 */
export default function ImageBlock({ entry }: { entry: ThreadEntry }) {
  const job = imageJob(entry.payload);
  if (!job) return null;
  // Keyed by job: a block never carries one job's state to another.
  return <Block key={job.jobId} entry={entry} job={job} />;
}

function Block({ entry, job }: { entry: ThreadEntry; job: NonNullable<ReturnType<typeof imageJob>> }) {
  const t = useTranslate();
  const canvas = useCanvas();
  const conversation = useImageConversation();
  const boxRef = useRef<HTMLDivElement | null>(null);
  const openRef = useRef<HTMLButtonElement | null>(null);
  const near = useNear(boxRef);
  const { view, announced, checking, recheck } = useImageJob(job.jobId, conversation, near, entry.created);
  const { phase, artifact } = view;
  const bytes = useImageBytes(artifact, conversation, near);
  const elapsed = useElapsed(entry.created, phase === "running");
  const title = job.title.trim() || t("Image");

  // Focus returns to the picture when the canvas it opened is closed.
  const showing = canvas.open && !!artifact && canvas.active === artifact.id;
  const armed = useRef(false);
  const wasShowing = useRef(false);
  useEffect(() => {
    if (wasShowing.current && !canvas.open && armed.current) {
      armed.current = false;
      const at = document.activeElement;
      if (!at || at === document.body) openRef.current?.focus();
    }
    wasShowing.current = showing;
  }, [showing, canvas.open]);

  const open = () => {
    if (!artifact) return;
    armed.current = true;
    // A host that has not fed this conversation to the canvas yet (the member
    // chat feeds it once its thread loads) holds none for it; the drawer only
    // opens for the conversation it holds.
    const held = canvasSnapshot();
    if (held.conversation !== conversation) resetCanvas(conversation, null, held.surface);
    openArtifact({ ...artifact, title: job.title.trim() || artifact.title, aspect: job.aspect });
  };

  const long = elapsed !== null && elapsed >= LONG_WAIT_MS;
  const card = phase === "failed" || phase === "unclear";
  let note = "";
  if (announced === "finished") note = `${t("Your image is ready")}: ${title}`;
  else if (announced === "failed") note = `${t("This image couldn't be created")}. ${t("The image service turned it down.")}`;
  else if (announced === "unclear") note = `${t("We can't confirm this image yet")}. ${t("We lost contact with the image service. It may still finish.")}`;

  let inner;
  if (phase === "pending") inner = <Waiting />;
  else if (phase === "running") {
    inner = (
      <Waiting
        label={long ? t("Still creating your image") : t("Creating your image")}
        elapsed={elapsed}
        sub={
          long
            ? `${t("Taking longer than usual.")} ${t("It will appear here when it's ready.")}`
            : t("You can leave this page. It will appear here when it's ready.")
        }
      />
    );
  } else if (phase === "failed") {
    inner = (
      <div className="imgstate" data-tone="fail">
        <div>
          <b>{t("This image couldn't be created")}</b>
          <small>{t("The image service turned it down.")}</small>
        </div>
      </div>
    );
  } else if (phase === "unclear") {
    inner = (
      <div className="imgstate" data-tone="warn">
        <div>
          <b>{t("We can't confirm this image yet")}</b>
          <small>{t("We lost contact with the image service. It may still finish.")}</small>
        </div>
        <button type="button" className="imgact" disabled={checking} onClick={() => void recheck()}>
          {t("Check again")}
        </button>
      </div>
    );
  } else if (phase === "unavailable" || bytes.status === "failed") {
    inner = <p className="imgquiet">{t("This image isn't available.")}</p>;
  } else if (bytes.status === "ready" && artifact) {
    inner = (
      <>
        <button ref={openRef} type="button" className="imgopen" aria-label={`${t("Open")} ${title}`} onClick={open}>
          <img src={bytes.src} alt={title} />
        </button>
        <ImageOverlay
          title={title}
          aspect={job.aspect}
          mime={bytes.mime}
          bytes={bytes.size}
          onDownload={() => saveBytes(dataUrlBytes(bytes.src), bytes.mime, downloadName(title, "image", bytes.mime))}
        />
      </>
    );
  } else inner = <Waiting />;

  return (
    <div className="imgblock" data-kind="image-job" data-job={job.jobId} data-shape={job.aspect} data-state={phase}>
      <div ref={boxRef} className="imgbox" data-compact={card || undefined}>
        {inner}
      </div>
      <span className="sr-only" role="status">
        {note}
      </span>
    </div>
  );
}
