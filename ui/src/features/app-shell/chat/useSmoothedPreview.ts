import { useEffect, useState } from "react";

function isHighSurrogate(code: number): boolean {
  return code >= 0xd800 && code <= 0xdbff;
}

function isLowSurrogate(code: number): boolean {
  return code >= 0xdc00 && code <= 0xdfff;
}

function safeEndIndex(text: string, candidate: number): number {
  const clamped = Math.max(0, Math.min(text.length, candidate));
  if (
    clamped > 0 &&
    clamped < text.length &&
    isHighSurrogate(text.charCodeAt(clamped - 1)) &&
    isLowSurrogate(text.charCodeAt(clamped))
  ) {
    return clamped + 1;
  }
  return clamped;
}

function prefersReducedMotion(): boolean {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return false;
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

/**
 * First visible slice painted on the opening non-empty preview frame so the
 * reader sees immediate movement (word-aligned around 8-16 chars) without a
 * blank-frame gap or split surrogate pair.
 */
export function firstPreviewSlice(text: string): string {
  if (text.length <= 16) return text;
  let leadEnd = 0;
  while (leadEnd < text.length && /\s/.test(text[leadEnd]!)) {
    leadEnd += 1;
  }
  const targetMin = Math.min(text.length, Math.max(12, leadEnd + 8));
  const limit = safeEndIndex(text, Math.min(text.length, targetMin + 4));
  let end = safeEndIndex(text, targetMin);
  while (end < limit && !/\s/.test(text[end]!)) {
    end = safeEndIndex(text, end + 1);
  }
  const slice = text.slice(0, end);
  if (slice.trim() === "") {
    let firstNonSpace = leadEnd;
    while (firstNonSpace < text.length && !/\s/.test(text[firstNonSpace]!)) {
      firstNonSpace = safeEndIndex(text, firstNonSpace + 1);
    }
    return text.slice(0, firstNonSpace);
  }
  return slice;
}

/**
 * Advance `shown` smoothly toward `target` over `elapsedMs`. Snaps immediately
 * if `target` shrank or diverged (e.g. redaction, quarantine, or stream reset).
 */
export function advancePreviewReveal(shown: string, target: string, elapsedMs: number): string {
  if (target.length <= shown.length || !target.startsWith(shown)) return target;
  const remaining = target.length - shown.length;
  const clampedMs = Math.min(64, Math.max(8, Number.isFinite(elapsedMs) ? elapsedMs : 16));
  const step = Math.max(2, Math.ceil((remaining * clampedMs) / 160));
  let nextEnd = safeEndIndex(target, shown.length + step);
  const wordSnapLimit = Math.min(target.length, nextEnd + 4);
  for (let probe = nextEnd; probe < wordSnapLimit; probe = safeEndIndex(target, probe + 1)) {
    if (/\s/.test(target[probe]!)) {
      nextEnd = probe + 1;
      break;
    }
  }
  return target.slice(0, Math.min(target.length, safeEndIndex(target, nextEnd)));
}

/**
 * Smooth chunked WS/SSE preview text across animation frames so multi-token
 * bursts reveal like natural typing while honoring `prefers-reduced-motion`
 * and snapping immediately on redaction or turn settlement.
 */
export function useSmoothedPreview(text: string | null, active: boolean): string | null {
  const bypass =
    typeof window === "undefined" ||
    typeof requestAnimationFrame !== "function" ||
    prefersReducedMotion();

  const [displayed, setDisplayed] = useState<string | null>(() => {
    if (!active || text === null) return null;
    if (bypass || text.trim() === "") return text;
    return firstPreviewSlice(text);
  });

  useEffect(() => {
    if (!active || text === null) {
      setDisplayed(null);
      return;
    }
    if (bypass || text.trim() === "") {
      setDisplayed(text);
      return;
    }
    let current = displayed;
    if (current === null) {
      current = firstPreviewSlice(text);
      setDisplayed(current);
    } else if (!text.startsWith(current) || text.length < current.length) {
      current = text;
      setDisplayed(current);
    }
    if (current === text) return;

    let frameId = 0;
    let cancelled = false;
    let lastTs: number | null = null;
    const step = (ts: number) => {
      if (cancelled) return;
      const elapsed = lastTs === null ? 16 : ts - lastTs;
      lastTs = ts;
      setDisplayed((prev) => {
        const base = prev === null ? firstPreviewSlice(text) : prev;
        const next = advancePreviewReveal(base, text, elapsed);
        if (next !== text && !cancelled) {
          frameId = requestAnimationFrame(step);
        }
        return next;
      });
    };
    frameId = requestAnimationFrame(step);
    return () => {
      cancelled = true;
      cancelAnimationFrame(frameId);
    };
  }, [active, text, bypass]);

  if (!active || text === null) return null;
  if (bypass || text.trim() === "") return text;
  if (displayed === null) return firstPreviewSlice(text);
  if (!text.startsWith(displayed) || text.length < displayed.length) return text;
  return displayed;
}
