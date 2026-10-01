import { useEffect, useRef, useState, type ReactNode } from "react";

/** Hidden scrollbars retain keyboard/touch scrolling; arrows expose overflow. */
export function HorizontalStrip({
  label,
  children,
  className = "",
  scrollStep,
}: {
  label: string;
  children: ReactNode;
  className?: string;
  /** Optional explicit scroll step in px (e.g. one card + gap). Default keeps
   *  the existing `Math.max(240, clientWidth * 0.8)` viewport behaviour. */
  scrollStep?: number;
}) {
  const strip = useRef<HTMLDivElement>(null);
  const [edges, setEdges] = useState({ before: false, after: false });
  useEffect(() => {
    const element = strip.current;
    if (!element) return;
    const update = () =>
      setEdges({
        before: element.scrollLeft > 1,
        after:
          element.scrollLeft + element.clientWidth < element.scrollWidth - 1,
      });
    update();
    element.addEventListener("scroll", update, { passive: true });
    const observer = new ResizeObserver(update);
    observer.observe(element);
    return () => {
      element.removeEventListener("scroll", update);
      observer.disconnect();
    };
  }, [children]);
  const move = (direction: number) => {
    const element = strip.current;
    if (!element) return;
    element.scrollBy({
      left: direction * (scrollStep ?? Math.max(240, element.clientWidth * 0.8)),
      behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches
        ? "auto"
        : "smooth",
    });
  };
  return (
    <div className="wa-strip-wrap">
      <div
        ref={strip}
        className={`wa-strip ${className}`}
        role="region"
        aria-label={label}
        tabIndex={0}
      >
        {children}
      </div>
      <button
        type="button"
        className="wa-scroll-arrow wa-scroll-before"
        hidden={!edges.before}
        aria-label={`Scroll back in ${label}`}
        onClick={() => move(-1)}
      >
        ‹
      </button>
      <button
        type="button"
        className="wa-scroll-arrow wa-scroll-after"
        hidden={!edges.after}
        aria-label={`Scroll forward in ${label}`}
        onClick={() => move(1)}
      >
        ›
      </button>
    </div>
  );
}
