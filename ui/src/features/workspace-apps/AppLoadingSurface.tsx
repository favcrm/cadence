export type AppLoadingVariant = "page" | "drawer" | "canvas";

/**
 * Calm, layout-neutral loading surface shared across app shell, installed
 * screen iframe mount, workspace app, and canvas app views (CAD-2534).
 * Always mounted immediately for screen readers and synchronous DOM checks;
 * visual presentation reveals gently after 120ms so fast loads stay invisible.
 */
export default function AppLoadingSurface({
  label,
  variant = "page",
  className,
}: {
  label: string;
  variant?: AppLoadingVariant;
  className?: string;
}) {
  const classes = [
    "wa-loading-surface",
    `wa-loading-surface--${variant}`,
    className ?? "",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <div className={classes} role="status" aria-label={label}>
      <span className="sr-only">{label}</span>
      <div className="wa-loading-header">
        <span className="wa-loading-bar wa-skel-pulse" style={{ width: variant === "drawer" ? "48%" : "11rem", height: "1rem" }} />
        {variant === "page" && (
          <span className="wa-loading-bar wa-skel-pulse" style={{ width: "5.5rem", height: "0.85rem", marginLeft: "auto" }} />
        )}
      </div>
      {variant === "page" ? (
        <div className="wa-loading-blocks">
          <div className="wa-loading-card">
            <span className="wa-loading-bar wa-skel-pulse" style={{ width: "36%", height: "0.8rem" }} />
            <span className="wa-loading-bar wa-skel-pulse" style={{ width: "88%", height: "2.25rem" }} />
            <span className="wa-loading-bar wa-skel-pulse" style={{ width: "72%", height: "2.25rem" }} />
          </div>
          <div className="wa-loading-card">
            <span className="wa-loading-bar wa-skel-pulse" style={{ width: "42%", height: "0.8rem" }} />
            <span className="wa-loading-bar wa-skel-pulse" style={{ width: "92%", height: "4.25rem" }} />
          </div>
        </div>
      ) : variant === "drawer" ? (
        <div className="wa-loading-rows">
          <span className="wa-loading-bar wa-skel-pulse" style={{ width: "86%", height: "2rem" }} />
          <span className="wa-loading-bar wa-skel-pulse" style={{ width: "94%", height: "3.25rem" }} />
          <span className="wa-loading-bar wa-skel-pulse" style={{ width: "68%", height: "2rem" }} />
        </div>
      ) : (
        <div className="wa-loading-card">
          <span className="wa-loading-bar wa-skel-pulse" style={{ width: "84%", height: "2rem" }} />
          <span className="wa-loading-bar wa-skel-pulse" style={{ width: "64%", height: "2rem" }} />
        </div>
      )}
    </div>
  );
}
