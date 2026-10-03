import type { ReactNode } from "react";
import { SESSION_EXPIRED_TEXT } from "./hostErrors";

/**
 * Shared record/form state blocks (CAD-866): the loading, error,
 * empty and read-only/notice surfaces every host app view repeats.
 * They exist so the accessible roles and the canonical tokens live in
 * exactly one place instead of being re-typed per screen.
 *
 * Live-region urgency follows modern-web-guidance:
 *  - `Loading`/`EmptyState` are `role="status"` (polite) — the screen
 *    reader is told the outcome at the next graceful break.
 *  - `ErrorNotice` is `role="alert"` (assertive) — a refusal that blocks
 *    the operator interrupts immediately.
 *  - `Notice` is a neutral bordered card — read-only/sign-in/context
 *    hints are plain content, announced only when reached.
 *
 * Every block is text-first and colour-independent — the message itself
 * carries the state, the chip/border colour only reinforces it.
 */

/** A neutral bordered notice card (read-only, sign-in, pick-a-context). */
export function Notice({
  children,
  className,
  state,
}: {
  children: ReactNode;
  className?: string;
  /** Optional `data-state` marker (e.g. "read-only") for tests/QA. */
  state?: string;
}) {
  return (
    <p
      className={["snotice", "card", "px-4", "py-3", "text-label", "text-ink-400", className ?? ""]
        .filter(Boolean)
        .join(" ")}
      data-state={state}
    >
      {children}
    </p>
  );
}

/** A polite busy line — "Reading customers…". */
export function Loading({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <p
      className={["sloading", "text-secondary", "text-ink-400", className ?? ""]
        .filter(Boolean)
        .join(" ")}
      role="status"
    >
      {children}
    </p>
  );
}

/**
 * A refusal message. `role="alert"` announces it; `onRetry` renders an
 * inline text-button Retry when the underlying call can be repeated.
 * The default is a bordered card; `bare` drops the card chrome for an
 * inline form-level error that sits inside an already-carded form.
 */
export function ErrorNotice({
  children,
  onRetry,
  retryLabel = "Retry",
  bare,
  className,
}: {
  children: ReactNode;
  onRetry?: () => void;
  retryLabel?: ReactNode;
  /** Inline form errors drop the card border/padding. */
  bare?: boolean;
  className?: string;
}) {
  return (
    <p
      className={[
        "serror",
        bare ? "" : "card px-4 py-3 border-fail/40",
        "text-label",
        "text-fail",
        className ?? "",
      ]
        .filter(Boolean)
        .join(" ")}
      role="alert"
    >
      {children}
      {onRetry || children === SESSION_EXPIRED_TEXT ? (
        <>
          {" "}
          {children === SESSION_EXPIRED_TEXT ? (
            // A top-level navigation re-authenticates; a retried fetch cannot.
            <button type="button" className="lnk" onClick={() => window.location.reload()}>
              Reload
            </button>
          ) : (
            <button type="button" className="lnk" onClick={onRetry}>
              {retryLabel}
            </button>
          )}
        </>
      ) : null}
    </p>
  );
}

/**
 * The empty-record card. `title` is the bolded line; `children` is the
 * supporting explanation. `name` sets `data-empty` for tests/QA.
 */
export function EmptyState({
  title,
  children,
  name,
  className,
}: {
  title: ReactNode;
  children?: ReactNode;
  /** `data-empty` marker (e.g. "customers") for tests/QA. */
  name?: string;
  className?: string;
}) {
  return (
    <div
      className={["sempty", "card", "px-4", "py-5", "text-secondary", "text-ink-400", className ?? ""]
        .filter(Boolean)
        .join(" ")}
      data-empty={name}
      role="status"
    >
      <p className="font-medium text-ink-200">{title}</p>
      {children !== undefined && children !== null ? <p className="mt-1">{children}</p> : null}
    </div>
  );
}
