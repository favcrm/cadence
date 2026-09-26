import type { MouseEvent, ReactNode } from "react";
import { navigate } from "../lib/useLocation";
import { activateButtonHref } from "./buttonActivate";

export type ButtonVariant = "primary" | "secondary" | "ghost" | "danger";
export type ButtonSize = "sm" | "md";

export type ButtonProps = {
  /** Visual weight. `secondary` is the bordered default. */
  variant?: ButtonVariant;
  /** `sm` is 1.75rem, `md` is 2rem. Text is centred in that fixed height. */
  size?: ButtonSize;
  /** Stretch to the container width. Label still centres, and overflows with an ellipsis. */
  full?: boolean;
  /** Disables the control and shows a spinner. Clicks do not fire. */
  loading?: boolean;
  icon?: ReactNode;
  disabled?: boolean;
  /**
   * Renders an anchor. A plain left click calls `navigate` (no full reload).
   * Modified clicks keep the browser default so the href can open in a new tab.
   */
  href?: string;
  /** Passed to `navigate` when `href` is set. */
  replace?: boolean;
  /** Ignored when `href` is set. Defaults to `button` so a form is not submitted by accident. */
  type?: "button" | "submit" | "reset";
  title?: string;
  className?: string;
  id?: string;
  children?: ReactNode;
  onClick?: (event: MouseEvent<HTMLButtonElement | HTMLAnchorElement>) => void;
  "aria-label"?: string;
};

function buttonClass(props: ButtonProps): string {
  const variant = props.variant ?? "secondary";
  return [
    "btn",
    variant === "primary" ? "btn-primary" : "",
    variant === "ghost" ? "btn-ghost" : "",
    variant === "danger" ? "btn-danger" : "",
    props.size === "sm" ? "btn-sm" : "",
    props.full ? "btn-full" : "",
    props.className ?? "",
  ]
    .filter(Boolean)
    .join(" ");
}

function labelTitle(title: string | undefined, children: ReactNode): string | undefined {
  if (title) return title;
  return typeof children === "string" && children.trim() ? children : undefined;
}

function ButtonBody({ icon, loading, children }: Pick<ButtonProps, "icon" | "loading" | "children">) {
  return (
    <>
      {loading ? <span className="btn-spin" aria-hidden="true" /> : null}
      {!loading && icon ? <span className="btn-icon">{icon}</span> : null}
      {children != null && children !== false ? <span className="btn-label">{children}</span> : null}
    </>
  );
}

/**
 * Shared button. Label text is centred (`justify-content` and `line-height: 1`
 * in `.btn`) so a full-width control does not sit on the left of its box.
 */
export function Button(props: ButtonProps) {
  const {
    disabled,
    loading,
    href,
    replace,
    type = "button",
    title,
    id,
    children,
    onClick,
    icon,
  } = props;
  const inactive = disabled || loading;
  const className = buttonClass(props);
  const tip = labelTitle(title, children);
  const ariaLabel = props["aria-label"];

  if (href) {
    return (
      <a
        id={id}
        href={inactive ? undefined : href}
        className={className}
        title={tip}
        aria-label={ariaLabel}
        aria-disabled={inactive || undefined}
        aria-busy={loading || undefined}
        tabIndex={inactive ? -1 : undefined}
        onClick={(event) => {
          onClick?.(event);
          if (inactive) event.preventDefault();
          activateButtonHref(href, event, navigate, replace ? { replace: true } : undefined);
        }}
      >
        <ButtonBody icon={icon} loading={loading}>
          {children}
        </ButtonBody>
      </a>
    );
  }

  return (
    <button
      id={id}
      type={type}
      className={className}
      title={tip}
      aria-label={ariaLabel}
      aria-busy={loading || undefined}
      disabled={inactive || undefined}
      onClick={onClick}
    >
      <ButtonBody icon={icon} loading={loading}>
        {children}
      </ButtonBody>
    </button>
  );
}

export default Button;
