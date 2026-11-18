import { forwardRef, type HTMLAttributes, type ReactNode, type Ref } from "react";

/**
 * CAD-2440 (card system phase 0, CAD-2437): the one frame for rich content
 * under a chat answer. Its header has a kind icon, a kind label, a subject
 * and an optional status; it comes in two shapes:
 *
 * - `shape="region"` (default): a labelled region — header, body and
 *   optional footer actions. The A2UI form uses it.
 * - `shape="button"`: the whole card is one control that summarises a thing —
 *   the kind icon as a tile, the subject as its title, meta lines and a
 *   trailing cue. The artifact and Browser cards use it.
 *
 * Colours come from board tokens only, so both shapes hold in dark and light.
 * A status is spoken as text, never as colour alone. Callers keep their own
 * class names and `data-*` hooks through `className` and the passed props.
 * The classes are `turncard-*`: `.tcard` is the Projects board's card.
 */

export type CardTone = "ok" | "warn" | "fail" | "info" | "accent" | "muted";
export type CardDensity = "full" | "compact";

export type CardStatus = {
  text: string;
  tone: CardTone;
  /** Extra attributes for the pill (a caller's own test hook). */
  attrs?: Record<string, string | undefined>;
};

type Shared = Omit<HTMLAttributes<HTMLElement>, "title"> & {
  kind: string;
  /** The kind icon (a region's header) or the tile (a button card). */
  icon?: ReactNode;
  /** What the card is about: a region's header line, a button card's title. */
  subject?: ReactNode;
  density?: CardDensity;
};

export type TurnCardRegionProps = Shared & {
  shape?: "region";
  /** The region's accessible name. */
  label: string;
  kindLabel?: ReactNode;
  status?: CardStatus;
  footer?: ReactNode;
  /** Class for the body wrapper, kept for callers whose layout keys off it. */
  bodyClassName?: string;
  children?: ReactNode;
};

export type TurnCardButtonProps = Shared & {
  shape: "button";
  meta?: ReactNode;
  trailing?: ReactNode;
  /** A real `<button>`, or a `role="button"` element for a caller that needs one. */
  element?: "button" | "div";
  open?: boolean;
};

export type TurnCardProps = TurnCardRegionProps | TurnCardButtonProps;

const join = (...names: (string | undefined | false)[]) => names.filter(Boolean).join(" ");

export const TurnCard = forwardRef<HTMLElement, TurnCardProps>(function TurnCard(props, ref) {
  if (props.shape === "button") {
    const { shape: _shape, kind, icon, subject, meta, trailing, density = "full", element = "button", open, className, ...rest } = props;
    const body = (
      <>
        <span className="turncard-tile arttype">{icon}</span>
        <span className="turncard-text arttext">
          <b>{subject}</b>
          {meta}
        </span>
        {trailing}
      </>
    );
    const shared = {
      ...rest,
      className: join("turncard-button artcard", className),
      "data-card-kind": kind,
      "data-density": density,
      "data-open": open || undefined,
    };
    return element === "button" ? (
      <button ref={ref as Ref<HTMLButtonElement>} type="button" {...shared}>
        {body}
      </button>
    ) : (
      <div ref={ref as Ref<HTMLDivElement>} {...shared}>
        {body}
      </div>
    );
  }
  const {
    shape: _shape,
    label,
    kind,
    icon,
    kindLabel,
    subject,
    status,
    footer,
    density = "full",
    bodyClassName,
    className,
    children,
    ...rest
  } = props;
  const hasHead = icon !== undefined || kindLabel !== undefined || subject !== undefined || status !== undefined;
  return (
    <section
      ref={ref}
      {...rest}
      className={join("turncard", className)}
      data-card-kind={kind}
      data-density={density}
      aria-label={label}
    >
      <div className={join("turncard-body", bodyClassName)}>
        {hasHead && (
          <div className="turncard-head">
            {(icon !== undefined || kindLabel !== undefined) && (
              <span className="turncard-kind">
                {icon}
                {kindLabel}
              </span>
            )}
            {subject !== undefined && <span className="turncard-subject">{subject}</span>}
            {status && (
              <span className="turncard-status" data-tone={status.tone} {...status.attrs}>
                {status.text}
              </span>
            )}
          </div>
        )}
        {children}
      </div>
      {footer !== undefined && footer !== null && footer !== false && <div className="turncard-foot">{footer}</div>}
    </section>
  );
});
