/* ScDrawer — the app's side drawer, same chrome as the board's issue
 * drawer (features/projects/Drawer.tsx): dimmed scrim + right aside,
 * Esc closes. Optional ‹ › arrows page through a list (Library's source
 * drawer). Arrow keys are ignored while typing in a field. */
import { useEffect } from "react";
import type { ReactNode } from "react";

export default function ScDrawer({
  title,
  sub,
  onClose,
  onPrev,
  onNext,
  children,
}: {
  title: ReactNode;
  sub?: ReactNode;
  onClose: () => void;
  onPrev?: () => void;
  onNext?: () => void;
  children: ReactNode;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
        return;
      }
      const el = e.target as HTMLElement | null;
      if (el && /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName)) return;
      if (e.key === "ArrowLeft" && onPrev) onPrev();
      else if (e.key === "ArrowRight" && onNext) onNext();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose, onPrev, onNext]);

  return (
    <>
      <div className="fixed inset-0 bg-scrim z-20" onClick={onClose} />
      <aside
        className="drawer fixed top-0 right-0 h-full w-full sm:w-[30rem] bg-ink-875 border-l border-ink-700 z-30 flex flex-col"
        role="dialog"
        aria-modal="true"
      >
        <header className="px-5 pt-4 pb-4 border-b border-ink-700 flex items-start gap-3 shrink-0">
          <div className="min-w-0 flex-1">
            {sub && <div className="num text-label text-ink-500">{sub}</div>}
            <h2 className="text-drawer font-semibold text-ink-100 leading-tight mt-1">{title}</h2>
          </div>
          {onPrev && (
            <button className={SC_DNAV} onClick={onPrev} aria-label="previous post">
              ‹
            </button>
          )}
          {onNext && (
            <button className={SC_DNAV} onClick={onNext} aria-label="next post">
              ›
            </button>
          )}
          <button
            aria-label="Close"
            onClick={onClose}
            className="closebtn shrink-0 w-8 h-8 grid place-items-center rounded border border-ink-600 text-ink-300 bg-ink-850"
          >
            <svg
              width="12"
              height="12"
              viewBox="0 0 12 12"
              stroke="currentColor"
              strokeWidth="1.5"
              fill="none"
              style={{ pointerEvents: "none" }}
            >
              <path d="M2 2l8 8M10 2l-8 8" />
            </svg>
          </button>
        </header>
        <div className="flex-1 overflow-y-auto px-5 py-5 flex flex-col gap-5">{children}</div>
      </aside>
    </>
  );
}

const SC_DNAV =
  "shrink-0 w-8 h-8 grid place-items-center rounded border border-ink-600 text-ink-300 bg-ink-850 hover:border-accent hover:text-accent text-base leading-none";
