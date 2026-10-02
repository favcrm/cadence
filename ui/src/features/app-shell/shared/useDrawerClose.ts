import { useCallback, useEffect, useRef, useState, type TransitionEvent } from "react";

/**
 * CAD-1013 coordinated drawer close. The CRM detail drawer mounts
 * directly on the owning list's selection (`{id !== null && <Drawer
 * onClose={() => onSelect(null)} />}`). To animate close as well as
 * open without restructuring each owner, the drawer marks itself
 * `data-closing`, lets the CSS exit transition run, then calls the
 * real `onClose` when it ends. Reduced-motion and browsers that skip
 * the transition fall back to closing on a timer, so the drawer can
 * never stick open. Single instance and draft are untouched — this
 * only delays the unmount by one transition.
 */
export function useDrawerClose(onClose: () => void): {
  closing: boolean;
  requestClose: () => void;
  onTransitionEnd: (e: TransitionEvent) => void;
} {
  const [closing, setClosing] = useState(false);
  const timer = useRef<number | null>(null);
  const done = useRef(false);

  const finish = useCallback(() => {
    if (done.current) return;
    done.current = true;
    onClose();
  }, [onClose]);

  const requestClose = useCallback(() => {
    if (closing || done.current) return;
    setClosing(true);
    // Safety net: if no transition runs (reduced-motion, no CSS), the
    // close still lands on the next tick — never a stuck-open drawer.
    timer.current = window.setTimeout(finish, 320);
  }, [closing, finish]);

  const onTransitionEnd = useCallback(
    (e: TransitionEvent) => {
      if (!closing) return;
      // Only the drawer's own transform/visibility transition ends the
      // close; a child transitioning must not finish it early.
      if (e.target !== e.currentTarget) return;
      finish();
    },
    [closing, finish],
  );

  useEffect(
    () => () => {
      if (timer.current !== null) window.clearTimeout(timer.current);
    },
    [],
  );

  return { closing, requestClose, onTransitionEnd };
}
