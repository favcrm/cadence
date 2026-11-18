import { forwardRef, type HTMLAttributes, type ReactNode } from "react";

/**
 * CAD-2440 (card system phase 0, CAD-2437): one frame for rich content under
 * a chat answer. Two shapes share it:
 *
 * - `TurnCard`: a labelled region with a header (kind icon, kind label,
 *   optional status), a body and optional footer actions. The A2UI form uses it.
 * - `TurnCardButton`: a whole-card button that summarises one thing (a kind
 *   tile, a title, meta lines, a trailing cue). The artifact and Browser cards
 *   use it.
 *
 * Colours come from board tokens only, so both hold in dark and light. A
 * status is spoken as text, never as colour alone. Callers keep their own
 * class names and `data-*` hooks through `className` and the passed props.
 */

export type CardTone = "ok" | "warn" | "fail" | "info" | "accent" | "muted";
export type CardDensity = "full" | "compact";

export type CardStatus = {
  text: string;
  tone: CardTone;
  /** Extra attributes for the pill (a caller's own test hook). */
  attrs?: Record<string, string | undefined>;
};

type TurnCardProps = Omit<HTMLAttributes<HTMLElement>, "title"> & {
  /** The region's accessible name. */
  label: string;
  kind: string;
  icon?: ReactNode;
  kindLabel?: ReactNode;
  status?: CardStatus;
  footer?: ReactNode;
  density?: CardDensity;
  /** Class for the body wrapper, kept for callers whose layout keys off it. */
  bodyClassName?: string;
  children?: ReactNode;
};

export const TurnCard = forwardRef<HTMLElement, TurnCardProps>(function TurnCard(
  { label, kind, icon, kindLabel, status, footer, density = "full", bodyClassName, className, children, ...rest },
  ref,
) {
  return (
    <section
      ref={ref}
      {...rest}
      className={["tcard", className].filter(Boolean).join(" ")}
      data-card-kind={kind}
      data-density={density}
      aria-label={label}
    >
      <div className={["tcard-body", bodyClassName].filter(Boolean).join(" ")}>
        {(icon !== undefined || kindLabel !== undefined || status !== undefined) && (
          <div className="tcard-head">
            {(icon !== undefined || kindLabel !== undefined) && (
              <span className="tcard-kind">
                {icon}
                {kindLabel}
              </span>
            )}
            {status && (
              <span className="tcard-status" data-tone={status.tone} {...status.attrs}>
                {status.text}
              </span>
            )}
          </div>
        )}
        {children}
      </div>
      {footer !== undefined && footer !== null && footer !== false && <div className="tcard-foot">{footer}</div>}
    </section>
  );
});

type TurnCardButtonProps = Omit<HTMLAttributes<HTMLElement>, "title"> & {
  kind: string;
  /** The kind tile: a short type label or an icon. */
  tile: ReactNode;
  title: ReactNode;
  meta?: ReactNode;
  trailing?: ReactNode;
  density?: CardDensity;
  /** A real `<button>`, or a `role="button"` element for a caller that needs one. */
  element?: "button" | "div";
  open?: boolean;
};

export function TurnCardButton({
  kind,
  tile,
  title,
  meta,
  trailing,
  density = "full",
  element = "button",
  open,
  className,
  ...rest
}: TurnCardButtonProps) {
  const body = (
    <>
      <span className="tcard-tile arttype">{tile}</span>
      <span className="tcard-text arttext">
        <b>{title}</b>
        {meta}
      </span>
      {trailing}
    </>
  );
  const shared = {
    ...rest,
    className: ["tcard-button artcard", className].filter(Boolean).join(" "),
    "data-card-kind": kind,
    "data-density": density,
    "data-open": open || undefined,
  };
  return element === "button" ? (
    <button type="button" {...shared}>
      {body}
    </button>
  ) : (
    <div {...shared}>{body}</div>
  );
}
