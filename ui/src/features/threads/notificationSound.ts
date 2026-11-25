/**
 * Synthesizes a subtle two-tone notification chime using Web Audio when
 * `preferences.sound` is enabled. Fails silently if AudioContext is
 * unavailable or blocked by browser autoplay policy.
 */

let sharedCtx: AudioContext | null = null;

function audioContext(): AudioContext | null {
  if (typeof window === "undefined") return null;
  if (sharedCtx && sharedCtx.state !== "closed") return sharedCtx;
  const ctor =
    window.AudioContext ??
    (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
  if (typeof ctor !== "function") return null;
  try {
    sharedCtx = new ctor();
    return sharedCtx;
  } catch {
    return null;
  }
}

export function playNotificationChime(): void {
  const ctx = audioContext();
  if (!ctx) return;
  try {
    if (ctx.state === "suspended") {
      void ctx.resume().catch(() => undefined);
    }
    const now = ctx.currentTime;
    const playTone = (freq: number, startOffset: number, duration: number) => {
      const osc = ctx.createOscillator();
      const gain = ctx.createGain();
      osc.type = "sine";
      osc.frequency.setValueAtTime(freq, now + startOffset);
      gain.gain.setValueAtTime(0.0001, now + startOffset);
      gain.gain.exponentialRampToValueAtTime(0.06, now + startOffset + 0.012);
      gain.gain.exponentialRampToValueAtTime(0.0001, now + startOffset + duration);
      osc.connect(gain);
      gain.connect(ctx.destination);
      osc.start(now + startOffset);
      osc.stop(now + startOffset + duration + 0.01);
    };
    playTone(587.33, 0, 0.075);
    playTone(880, 0.08, 0.095);
  } catch {
    // Ignore autoplay/headless audio failures.
  }
}
