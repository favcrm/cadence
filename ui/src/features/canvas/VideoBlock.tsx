import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslate } from "../../lib/locale";
import { ApiError } from "../../lib/apiError";
import type { ThreadEntry } from "../home/thread";
import { sizeLabel } from "./ArtifactCard";
import { BACKOFF_MAX_MS, GIVE_UP_MS, POLL_MS, Waiting, clock, useElapsed, useNear } from "./ImageBlock";
import { useImageConversation } from "./imageConversation";
import { formatLabel } from "./ImageOverlay";
import { VIDEO_CAP_BYTES, readVideoJob, videoBytes, videoJob, type VideoJob, type VideoStatus } from "./videoJobClient";

/** PROVISIONAL: the point at which "Taking longer than usual" appears. A
 *  video takes minutes; the accepted design mocks about 10. Set it from
 *  measured durations once there are some. */
export const VIDEO_LONG_WAIT_MS = 10 * 60 * 1000;

type Phase = "pending" | "running" | "finished" | "failed" | "unclear" | "toolarge" | "unavailable";
interface View {
  phase: Phase;
  mime?: string;
  bytes?: number;
  duration?: number;
}

const TERMINAL: ReadonlySet<Phase> = new Set(["finished", "failed", "unclear", "toolarge", "unavailable"]);

/** What a status reads as. A job still running past `GIVE_UP_MS` reads as
 *  unconfirmed, and a finished one over the cap as too large. */
const viewOf = (status: VideoStatus, created: string | null | undefined): View => {
  if (status.state === "finished") {
    if (status.bytes > VIDEO_CAP_BYTES) return { phase: "toolarge" };
    return { phase: "finished", mime: status.mime, bytes: status.bytes, duration: status.duration };
  }
  if (status.state === "too_large") return { phase: "toolarge" };
  const start = typeof created === "string" ? Date.parse(created) : NaN;
  if (status.state === "running" && !Number.isNaN(start) && Date.now() - start >= GIVE_UP_MS) return { phase: "unclear" };
  return { phase: status.state };
};

/**
 * What the host says of one video job, read the way the image block reads
 * its job: nothing is invented (until a read succeeds the phase is `pending`),
 * the first read waits for the box to be near the viewport, and polling stops
 * while the page is hidden.
 */
function useVideoJob(jobId: string, conversation: string | null, near: boolean, created: string | null | undefined) {
  const [view, setView] = useState<View>({ phase: "pending" });
  const [checking, setChecking] = useState(false);
  const [announced, setAnnounced] = useState<Phase | null>(null);
  const sawRunning = useRef(false);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const apply = useCallback((next: View) => {
    if (!mounted.current) return;
    if (next.phase === "running") sawRunning.current = true;
    if (TERMINAL.has(next.phase) && sawRunning.current) {
      sawRunning.current = false;
      setAnnounced(next.phase);
    }
    setView((cur) => (cur.phase === next.phase && cur.bytes === next.bytes ? cur : next));
  }, []);

  const live = view.phase === "pending" || view.phase === "running";
  useEffect(() => {
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
        const read = await readVideoJob(jobId, conversation, ctl.signal);
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
      const read = await readVideoJob(jobId, conversation);
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

type Media = { status: "idle" | "loading" | "retry" | "failed" } | { status: "ready"; url: string };

/** The video's bytes as a blob URL, fetched only when asked for (play or
 *  download), never on load. The board CSP allows `media-src blob:`. */
function useVideoMedia(jobId: string, conversation: string | null, mime: string | undefined, size: number | undefined) {
  const [media, setMedia] = useState<Media>({ status: "idle" });
  const urlRef = useRef<string | null>(null);
  const pending = useRef<Promise<string | null> | null>(null);
  const mounted = useRef(true);
  const abort = useRef<AbortController | null>(null);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      abort.current?.abort();
      if (urlRef.current) URL.revokeObjectURL(urlRef.current);
    };
  }, []);
  /** The browser could not decode what was fetched: drop it so a retry fetches again. */
  const reject = useCallback(() => {
    if (urlRef.current) URL.revokeObjectURL(urlRef.current);
    urlRef.current = null;
    setMedia({ status: "retry" });
  }, []);
  const load = useCallback((): Promise<string | null> => {
    if (urlRef.current) return Promise.resolve(urlRef.current);
    if (pending.current) return pending.current;
    if (conversation === null || !mime) return Promise.resolve(null);
    setMedia({ status: "loading" });
    const ctl = new AbortController();
    abort.current = ctl;
    const run = videoBytes(jobId, conversation, ctl.signal)
      .then((data) => {
        if (!mounted.current) return null;
        // No video without bytes that are one.
        if (data.mime !== mime) {
          setMedia({ status: "failed" });
          return null;
        }
        // The status said how large it is; anything else (empty, cut short) is not it.
        if (data.blob.size !== size) {
          setMedia({ status: "retry" });
          return null;
        }
        const url = URL.createObjectURL(data.blob.slice(0, data.blob.size, mime));
        urlRef.current = url;
        setMedia({ status: "ready", url });
        return url;
      })
      .catch((e: unknown) => {
        // Only a refusal or a missing video is final; a temporary fault can be tried again.
        if (mounted.current) setMedia({ status: e instanceof ApiError && [401, 403, 404].includes(e.status) ? "failed" : "retry" });
        return null;
      })
      .finally(() => {
        pending.current = null;
      });
    pending.current = run;
    return run;
  }, [conversation, jobId, mime, size]);
  return { media, load, reject };
}

const PLAY_ICON = (
  <svg viewBox="0 0 24 24" width="22" height="22" fill="currentColor" aria-hidden>
    <path d="M8 5v14l11-7z" />
  </svg>
);
const DOWNLOAD_ICON = (
  <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
    <path d="M12 4v11m0 0-4-4m4 4 4-4M5 20h14" />
  </svg>
);

/**
 * A generated video in the thread: one box at the video's shape from first
 * paint, so the player replaces the placeholder in place. It reads like the
 * image block (same states, polling and visibility). The thread entry carries
 * only a job id, title, shape and length; the host says how the job stands.
 * A finished video is a poster with a play button: the bytes are fetched into
 * a blob only when the user plays or downloads, and play is inline with the
 * browser's own controls. A video over the cap is its own card, with no
 * Check again, because asking again cannot make it smaller.
 */
export default function VideoBlock({ entry }: { entry: ThreadEntry }) {
  const job = videoJob(entry.payload);
  if (!job) return null;
  return <Block key={job.jobId} entry={entry} job={job} />;
}

function Block({ entry, job }: { entry: ThreadEntry; job: VideoJob }) {
  const t = useTranslate();
  const conversation = useImageConversation();
  const boxRef = useRef<HTMLDivElement | null>(null);
  const near = useNear(boxRef);
  const { view, announced, checking, recheck } = useVideoJob(job.jobId, conversation, near, entry.created);
  const { phase } = view;
  const { media, load, reject } = useVideoMedia(job.jobId, conversation, view.mime, view.bytes);
  const [playing, setPlaying] = useState(false);
  const playerRef = useRef<HTMLVideoElement | null>(null);
  const showPlayer = playing && media.status === "ready";
  // The button that was pressed is gone once the player replaces it.
  useEffect(() => {
    if (showPlayer) playerRef.current?.focus();
  }, [showPlayer]);
  const elapsed = useElapsed(entry.created, phase === "running");
  const title = job.title.trim() || t("Video");
  const length = clock((view.duration ?? job.duration) * 1000);

  const play = () => {
    setPlaying(true);
    void load().then((url) => {
      if (url === null) setPlaying(false);
    });
  };
  const download = () => {
    void load().then((url) => {
      if (url === null || !view.mime) return;
      const a = document.createElement("a");
      a.href = url;
      a.download = `${title.replace(/[^\w.\- ]+/g, "_").trim().slice(0, 80) || "video"}.${view.mime === "video/webm" ? "webm" : "mp4"}`;
      document.body.appendChild(a);
      a.click();
      a.remove();
    });
  };

  const long = elapsed !== null && elapsed >= VIDEO_LONG_WAIT_MS;
  const card = phase === "failed" || phase === "unclear" || phase === "toolarge";
  let note = "";
  if (announced === "finished") note = `${t("Your video is ready")}: ${title}`;
  else if (announced === "failed") note = `${t("This video couldn't be created")}. ${t("The video service turned it down.")}`;
  else if (announced === "unclear") note = `${t("We can't confirm this video yet")}. ${t("We lost contact with the video service. It may still finish.")}`;
  else if (announced === "toolarge") note = `${t("This video is too large to keep in the chat")}. ${t("Ask for a shorter or lower-resolution version in the chat.")}`;

  let inner;
  if (phase === "pending") inner = <Waiting />;
  else if (phase === "running") {
    inner = (
      <Waiting
        label={long ? t("Still creating your video") : t("Creating your video")}
        elapsed={elapsed}
        sub={
          long
            ? `${t("Taking longer than usual.")} ${t("Your video will appear here when it's ready.")}`
            : t("Videos take a few minutes. You can leave this page. It will appear here when it's ready.")
        }
      />
    );
  } else if (phase === "failed") {
    inner = (
      <div className="imgstate" data-tone="fail">
        <div>
          <b>{t("This video couldn't be created")}</b>
          <small>{t("The video service turned it down.")}</small>
        </div>
      </div>
    );
  } else if (phase === "unclear") {
    inner = (
      <div className="imgstate" data-tone="warn">
        <div>
          <b>{t("We can't confirm this video yet")}</b>
          <small>{t("We lost contact with the video service. It may still finish.")}</small>
        </div>
        <button type="button" className="imgact" disabled={checking} onClick={() => void recheck()}>
          {t("Check again")}
        </button>
      </div>
    );
  } else if (phase === "toolarge") {
    inner = (
      <div className="imgstate" data-tone="warn">
        <div>
          <b>{t("This video is too large to keep in the chat")}</b>
          <small>{t("It finished, but it is over the 100 MB the chat can keep.")}</small>
          <small>{t("Ask for a shorter or lower-resolution version in the chat.")}</small>
        </div>
      </div>
    );
  } else if (phase === "unavailable" || media.status === "failed") {
    inner = <p className="imgquiet">{t("This video isn't available.")}</p>;
  } else if (phase === "finished") {
    const details = [job.aspect === "auto" ? "" : job.aspect, formatLabel(view.mime ?? ""), length, sizeLabel(view.bytes ?? 0)].filter(Boolean).join(" · ");
    inner = (
      <>
        {showPlayer ? (
          <video ref={playerRef} className="vidplayer" src={media.url} onError={() => { setPlaying(false); reject(); }} controls autoPlay playsInline preload="metadata" aria-label={title} />
        ) : (
          <button type="button" className="vidopen" aria-label={`${t("Play")} ${title}`} aria-busy={media.status === "loading" || undefined} disabled={media.status === "loading"} onClick={play}>
            <span className="vidplay">{PLAY_ICON}</span>
            <span className="viddur num" aria-hidden>{length}</span>
          </button>
        )}
        <div className="vidover" role="group" aria-label={title}>
          <div className="imgcap">
            <b>{title}</b>
            <span>{details}</span>
          </div>
          <button type="button" className="imgtool" aria-label={t("Download")} title={t("Download")} disabled={media.status === "loading"} onClick={download}>
            {DOWNLOAD_ICON}
          </button>
        </div>
        {media.status === "retry" ? <p className="vidretry" role="status">{t("Couldn't load the video. Try again.")}</p> : null}
      </>
    );
  } else inner = <Waiting />;

  return (
    <div className="imgblock vidblock" data-kind="video-job" data-job={job.jobId} data-shape={job.aspect} data-state={phase}>
      <div ref={boxRef} className="imgbox vidbox" data-compact={card || undefined} data-playing={showPlayer || undefined}>
        {inner}
      </div>
      <span className="sr-only" role="status">
        {note}
      </span>
    </div>
  );
}
