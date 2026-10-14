import { createContext, useCallback, useContext, useMemo, useRef, useState, type ReactNode } from "react";
import Button from "../../ui/Button";
import { IconClose } from "../../ui/icons";
import { navigate } from "../../lib/useLocation";

/** One in-app alert. `href` is fixed when the alert is created: Open always goes to this thread. */
interface Alert {
  id: number;
  title: string;
  href: string;
}

export interface ToastApi {
  /** Announce a new message for `room`. Deduplicated per room by the caller's seq. */
  show: (title: string, room: string) => void;
  /** Drop every pending alert (identity change). */
  clear: () => void;
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

  const dismiss = useCallback((id: number) => {
    setAlerts((cur) => cur.filter((a) => a.id !== id));
  }, []);

  const clear = useCallback(() => {
    pending.current.clear();
    setAlerts([]);
  }, []);

  const show = useCallback(
    (title: string, room: string) => {
      const existing = pending.current.get(room);
      const id = ++seq.current;
      pending.current.set(room, id);
      const href = `/threads?h=${encodeURIComponent(room)}`;
      setAlerts((cur) => {
        const rest = cur.filter((a) => !(a.href === href && a.id === existing));
        return [...rest, { id, title, href }].slice(-3);
      });
      window.setTimeout(() => {
        if (pending.current.get(room) === id) pending.current.delete(room);
        dismiss(id);
      }, TOAST_MS);
    },
    [dismiss],
  );

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
              <p>Unread until you open it.</p>
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
