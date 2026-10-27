import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import Button from "../../ui/Button";
import { IconClose } from "../../ui/icons";
import { navigate } from "../../lib/useLocation";
import { onPrivateReadsPaused, privateReadsAllowed } from "../../lib/privateSession";

/** One in-app alert. `href` is fixed when the alert is created: Open always goes to this thread. */
interface Alert {
  id: number;
  title: string;
  href: string;
  note: string;
}

export interface ToastApi {
  /** Announce a new message for `room` (a Threads room unless `target.href` says where Open goes). Deduplicated per room by the caller's seq. */
  show: (title: string, room: string, target?: { href: string; note: string }) => void;
  /** Drop one room's alert after it is seen, or every alert on identity change. */
  clear: (room?: string) => void;
}

const ToastContext = createContext<ToastApi>({ show: () => undefined, clear: () => undefined });

/** How long an alert stays before it auto-dismisses. Dismiss leaves the message unread. */
const TOAST_MS = 7000;

export function NotificationToastProvider({ children }: { children: ReactNode }) {
  const [alerts, setAlerts] = useState<Alert[]>([]);
  const seq = useRef(0);
  // Per-room announce guard: one alert per room while it is pending, so a burst
  // of polls for one thread coalesces into the newest alert.
  const pending = useRef(new Map<string, number>());
  const timers = useRef(new Map<number, number>());
  const retired = useRef(false);

  const dismiss = useCallback((id: number) => {
    setAlerts((cur) => cur.filter((a) => a.id !== id));
  }, []);

  const dropTimers = useCallback(() => {
    for (const timer of timers.current.values()) window.clearTimeout(timer);
    timers.current.clear();
  }, []);

  const clear = useCallback((room?: string) => {
    if (room !== undefined) {
      const id = pending.current.get(room);
      if (id === undefined) return;
      pending.current.delete(room);
      const timer = timers.current.get(id);
      if (timer !== undefined) window.clearTimeout(timer);
      timers.current.delete(id);
      setAlerts((cur) => cur.filter((alert) => alert.id !== id));
      return;
    }
    dropTimers();
    pending.current.clear();
    setAlerts([]);
  }, [dropTimers]);

  const show = useCallback(
    (title: string, room: string, target?: { href: string; note: string }) => {
      if (retired.current || !privateReadsAllowed()) return;
      const existing = pending.current.get(room);
      const id = ++seq.current;
      pending.current.set(room, id);
      const href = target?.href ?? `/threads?h=${encodeURIComponent(room)}`;
      const note = target?.note ?? "Unread until you open it.";
      setAlerts((cur) => {
        if (retired.current || !privateReadsAllowed()) return cur;
        const rest = cur.filter((a) => !(a.href === href && a.id === existing));
        return [...rest, { id, title, href, note }].slice(-3);
      });
      const timer = window.setTimeout(() => {
        timers.current.delete(id);
        if (pending.current.get(room) === id) pending.current.delete(room);
        if (retired.current || !privateReadsAllowed()) return;
        dismiss(id);
      }, TOAST_MS);
      timers.current.set(id, timer);
    },
    [dismiss],
  );

  // Activity disconnects effects while retaining drafts; private alerts do not survive a pause.
  useEffect(() => {
    retired.current = !privateReadsAllowed();
    const stop = onPrivateReadsPaused(() => {
      retired.current = true;
      clear();
    });
    return () => {
      retired.current = true;
      stop();
      dropTimers();
    };
  }, [clear, dropTimers]);

  // Memoized so consumers' effects depend on a stable value, not on every alert change.
  const api = useMemo(() => ({ show, clear }), [show, clear]);

  return (
    <ToastContext.Provider value={api}>
      {children}
      <div className="fixed" aria-live="polite" style={{ right: 22, bottom: 22, zIndex: 50, display: "grid", gap: 8 }}>
        {alerts.map((a) => (
          <div key={a.id} className="notification-toast" role="status">
            <div>
              <strong>{a.title}</strong>
              <p>{a.note}</p>
              <button type="button" className="lnk" onClick={() => { dismiss(a.id); navigate(a.href); }}>Open</button>
            </div>
            <Button variant="ghost" size="sm" aria-label="Dismiss notification" icon={<IconClose />} onClick={() => dismiss(a.id)} />
          </div>
        ))}
      </div>
    </ToastContext.Provider>
  );
}

export function useNotificationToast(): ToastApi {
  return useContext(ToastContext);
}
