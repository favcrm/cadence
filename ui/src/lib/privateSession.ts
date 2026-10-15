/** In-memory admission and generation for private observations, never a credential. */
let admitted = true;
let generation = 0;
const resets = new Set<() => void>();
const pauses = new Set<() => void>();

export const privateReadsAllowed = (): boolean => admitted;
export const privateSessionGeneration = (): number => generation;

export function pausePrivateReads(): void {
  admitted = false;
  for (const pause of pauses) pause();
}

export function allowPrivateReads(): void {
  admitted = true;
}

/** Confirmed identity loss/change, not an unanswered verification. */
export function resetPrivateSession(): void {
  pausePrivateReads();
  generation += 1;
  for (const reset of resets) reset();
}

export function onPrivateSessionReset(reset: () => void): () => void {
  resets.add(reset);
  return () => { resets.delete(reset); };
}

export function onPrivateReadsPaused(pause: () => void): () => void {
  pauses.add(pause);
  return () => { pauses.delete(pause); };
}

const pendingReads = new Set<AbortController>();
onPrivateReadsPaused(() => {
  for (const controller of pendingReads) controller.abort();
  pendingReads.clear();
});

/** Independent feature transports share read admission and cancellation.
 * Callers retaining module-level data must also fence their decoded result. */
export async function privateFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
  if (!admitted) throw new Error("Session verification required");
  const started = generation;
  const controller = new AbortController();
  // Keep the caller's abort effective for the response body, not just headers.
  const signal = init?.signal ? AbortSignal.any([controller.signal, init.signal]) : controller.signal;
  pendingReads.add(controller);
  try {
    const response = await fetch(input, { ...init, signal });
    if (!admitted || started !== generation) throw new Error("Session changed during request");
    if (response.status === 401 && typeof window !== "undefined") {
      const body = await response.clone().json().catch(() => null);
      if (!admitted || started !== generation) throw new Error("Session changed during response");
      const code = body?.code ?? body?.error?.code;
      if (code === "operator_session_required" || code === "board_session_required") {
        window.dispatchEvent(new Event("cadence:session-required"));
      }
    }
    return response;
  } finally {
    pendingReads.delete(controller);
  }
}
