import { useEffect, useRef, useState } from "react";
import { describeRefusal, nextDelaySecs, safeVerificationHref, startDelaySecs } from "./deviceFlow";
import { pollDeviceSignIn, startDeviceSignIn } from "./session";
import type { DeviceDisplay } from "./session";

type Phase =
  | { kind: "idle" }
  | { kind: "waiting"; display: DeviceDisplay }
  | { kind: "failed"; reason: string };

/**
 * "Sign in with AgenticOS" (CAD-777): requests a device grant, shows the
 * user code plus the issuer's approval link, and polls until the grant
 * is approved, denied or expired. `pending_id` never renders — it only
 * feeds the poll calls.
 */
export default function DeviceSignIn({ onSignedIn }: { onSignedIn: () => void }) {
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const alive = useRef(true);

  const stop = () => {
    if (timer.current !== null) {
      clearTimeout(timer.current);
      timer.current = null;
    }
  };
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      stop();
    };
  }, []);

  const poll = (pendingId: string, delaySecs: number, deadline: number) => {
    if (!alive.current) return;
    if (Date.now() >= deadline) {
      setPhase({ kind: "failed", reason: "The grant expired — start again." });
      return;
    }
    timer.current = setTimeout(() => {
      pollDeviceSignIn(pendingId)
        .then((outcome) => {
          if (!alive.current) return;
          if (outcome.kind === "signed_in") {
            onSignedIn();
            return;
          }
          if (outcome.kind === "pending" || outcome.kind === "slow_down") {
            poll(pendingId, nextDelaySecs(delaySecs, outcome.kind), deadline);
            return;
          }
          setPhase({
            kind: "failed",
            reason:
              outcome.kind === "denied"
                ? "The approval was denied on AgenticOS."
                : "The grant expired — start again.",
          });
        })
        .catch((err: unknown) => {
          if (alive.current) setPhase({ kind: "failed", reason: describeRefusal(err) });
        });
    }, delaySecs * 1000);
  };

  const begin = () => {
    stop();
    startDeviceSignIn()
      .then((display) => {
        if (!alive.current) return;
        setPhase({ kind: "waiting", display });
        poll(display.pending_id, startDelaySecs(display), Date.now() + display.expires_in * 1000);
      })
      .catch((err: unknown) => {
        setPhase({ kind: "failed", reason: describeRefusal(err) });
      });
  };

  if (phase.kind === "idle") {
    return (
      <button
        type="button"
        onClick={begin}
        className="chip bg-accent/10 text-accent hover:bg-accent/20 transition-colors"
      >
        Sign in with AgenticOS
      </button>
    );
  }
  if (phase.kind === "failed") {
    return (
      <div>
        <p className="text-label text-warn" aria-live="polite">
          {phase.reason}
        </p>
        <button
          type="button"
          onClick={begin}
          className="chip bg-accent/10 text-accent hover:bg-accent/20 transition-colors mt-2"
        >
          Start again
        </button>
      </div>
    );
  }
  const { display } = phase;
  const href = safeVerificationHref(display);
  return (
    <div>
      <p className="text-label text-ink-400 mb-1">Enter this code on AgenticOS:</p>
      <p className="num text-lg text-ink-100 select-all">{display.user_code}</p>
      {href ? (
        <a
          href={href}
          target="_blank"
          rel="noopener noreferrer"
          className="text-label text-accent hover:underline inline-block mt-1"
        >
          Open AgenticOS approval
        </a>
      ) : (
        <p className="text-label text-warn mt-1" aria-live="polite">
          The issuer sent no usable approval link — enter the code there manually.
        </p>
      )}
      <p className="text-label text-ink-400 mt-2" aria-live="polite">
        Waiting for approval…
      </p>
    </div>
  );
}
