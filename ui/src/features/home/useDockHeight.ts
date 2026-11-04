import { useEffect, type RefObject } from "react";
import { atTail, DOCK_VAR } from "./dock";

/** Publish the dock's measured height, so the thread's bottom padding always
 *  clears it. A reader on the tail stays there when the dock grows: "on the
 *  tail" is judged before the new padding lands. */
export function useDockHeight(
  dockRef: RefObject<HTMLElement | null>,
  scrollRef: RefObject<HTMLElement | null>,
  keepTail: (onTail: boolean) => void,
): void {
  useEffect(() => {
    const dock = dockRef.current;
    if (!dock || typeof ResizeObserver === "undefined") return;
    const apply = () => {
      const el = scrollRef.current;
      const onTail = el ? atTail(el.scrollTop, el.scrollHeight, el.clientHeight) : false;
      document.documentElement.style.setProperty(DOCK_VAR, `${dock.offsetHeight}px`);
      keepTail(onTail);
    };
    const ro = new ResizeObserver(apply);
    ro.observe(dock);
    apply();
    return () => {
      ro.disconnect();
      document.documentElement.style.removeProperty(DOCK_VAR);
    };
  }, [dockRef, scrollRef, keepTail]);
}
