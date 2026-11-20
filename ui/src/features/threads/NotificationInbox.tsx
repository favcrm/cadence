import { useEffect, useRef, useState } from "react";
import "./notifications.css";
import Button from "../../ui/Button";
import { IconCheck, IconClose, IconSettings, IconThreads } from "../../ui/icons";
import { navigate, useHref } from "../../lib/useLocation";
import { useTranslate } from "../../lib/locale";
import { ApiError } from "../../lib/api";
import {
  humanNotifications,
  markNotificationsRead,
  setNotificationPreferences,
  setRoomMuted,
  type NotificationInbox as Inbox,
  type NotificationPreferences,
  type NotificationGroup,
} from "./humanThreadsClient";
import {
  SW_PATH,
  StaleEnableError,
  disableBrowserAlerts,
  enableBrowserAlerts,
  rebindExistingBrowserAlerts,
  pushSupported,
  reportBrowserPresence,
  releaseBrowserAlerts,
} from "./pushOptIn";
import { useNotificationToast } from "./NotificationToast";

/** The newest eligible unread alert intent already observed per room. */
const announced = new Map<string, number>();

/** Private inbox poll: authorized, private state only. Never shared SSE. */
const INBOX_POLL_MS = 15000;

const DEVICE_KEY = "cadence.push.device";

/** The thread route the Threads page reads (`?h=<room>`). */
export function threadHref(room: string): string {
  return `/threads?h=${encodeURIComponent(room)}`;
}

/** Visible-thread suppression: a message in the thread the user is viewing is read, never an alert. */
export function activeRoom(): string | null {
  if (typeof window === "undefined") return null;
  if (document.visibilityState !== "visible") return null;
  const path = window.location.pathname;
  if (path !== "/threads") return null;
  return new URLSearchParams(window.location.search).get("h");
}

/**
 * CAD-1423 approved A: the header notification popover. Rendered only for a
 * verified human (the server decides). `authEpoch` is the board's own auth
 * state; every change starts a new generation that resets the cache, dialog,
 * toasts and dedupe, and drops responses from the previous generation.
 */
export default function NotificationInbox({ authEpoch }: { authEpoch: string }) {
  const t = useTranslate();
  const tRef = useRef(t);
  useEffect(() => {
    tRef.current = t;
  }, [t]);
  const [inbox, setInbox] = useState<Inbox | null>(null);
  const [filter, setFilter] = useState<"unread" | "all">("unread");
  const [notice, setNotice] = useState<string | null>(null);
  const [preferenceNotice, setPreferenceNotice] = useState<string | null>(null);
  const [browserAlertNotice, setBrowserAlertNotice] = useState<string | null>(null);
  const [popoverOpen, setPopoverOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [deviceId, setDeviceId] = useState<string | null>(() => localStorage.getItem(DEVICE_KEY));
  const generation = useRef(0);
  // Last verified human. Kept across auth epochs so a switch to another account is caught on its first valid feed.
  const known = useRef<string | null>(null);
  const popover = useRef<HTMLDialogElement | null>(null);
  const settings = useRef<HTMLDialogElement | null>(null);
  const bell = useRef<HTMLButtonElement | null>(null);
  const toast = useNotificationToast();
  const href = useHref();
  const humanId = inbox?.human.id;

  useEffect(() => {
    const onStorage = (event: StorageEvent) => {
      if (event.storageArea === localStorage && event.key === DEVICE_KEY) {
        setDeviceId(event.newValue);
        setBrowserAlertNotice(null);
      }
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, []);

  // Click from the service worker: only our registered push worker, and only a
  // same-origin thread path, may route the board. Room authorization still applies.
  useEffect(() => {
    if (!("serviceWorker" in navigator)) return;
    const onMessage = (event: MessageEvent) => {
      const data = event.data as { type?: unknown; url?: unknown } | null;
      if (data?.type !== "cadence-open-thread" || typeof data.url !== "string") return;
      const src = event.source;
      if (!(src instanceof ServiceWorker)) return;
      if (new URL(src.scriptURL).pathname !== SW_PATH) return;
      if (!/^\/threads\?h=[A-Za-z0-9_.:%-]{1,128}$/.test(data.url)) return;
      navigate(data.url);
    };
    navigator.serviceWorker.addEventListener("message", onMessage);
    return () => navigator.serviceWorker.removeEventListener("message", onMessage);
  }, []);

  // Verified identity and private feed, one lifecycle per auth epoch. The feed
  // carries the verified human, so identity and data change together. A switch
  // of identity inside one epoch is also caught here and resets everything.
  useEffect(() => {
    ++generation.current;
    announced.clear();
    toast.clear();
    setInbox(null);
    setNotice(null);
    setPreferenceNotice(null);
    setBrowserAlertNotice(null);
    setSettingsOpen(false);
    setPopoverOpen(false);
    popover.current?.close();
    settings.current?.close();

    let latest = 0;
    let active = true;
    let timer = 0;
    let linked = false;
    let linking: Promise<void> | null = null;
    const linkBeforeFeed = () => {
      if (linked) return Promise.resolve();
      if (linking === null) {
        const epoch = generation.current;
        linking = rebindExistingBrowserAlerts(() => active && generation.current === epoch)
          .then(() => {
            if (active && generation.current === epoch) {
              linked = true;
              setDeviceId(localStorage.getItem(DEVICE_KEY));
            }
          })
          .catch((e: unknown) => {
            if (
              e instanceof StaleEnableError ||
              (e instanceof ApiError && (e.status === 401 || e.status === 403))
            ) {
              throw e;
            }
            if (active && generation.current === epoch) {
              setBrowserAlertNotice("Could not sync browser alerts. Your threads still load.");
            }
          })
          .finally(() => {
            linking = null;
          });
      }
      return linking;
    };
    const stop = () => window.clearInterval(timer);
    const releaseDevice = () => {
      // Serialized before any new enable, so clearing the id here cannot erase a newer one.
      void releaseBrowserAlerts()
        .then(() => {
          if (active) setDeviceId(null);
        })
        .catch(() => undefined);
    };
    // Each poll captures the live generation at send time, never the effect's
    // starting value, and only the newest request may apply its response.
    const load = () => {
      const sent = ++latest;
      const started = generation.current;
      humanNotifications()
        .then(async (next) => {
          if (!active || sent !== latest || generation.current !== started) return;
          if (known.current !== null && next.human.id !== known.current) {
            // A verified identity change inside one epoch starts a new generation:
            // in-flight actions from the old identity are dropped, not applied.
            generation.current++;
            announced.clear();
            toast.clear();
            setInbox(null);
            setNotice(null);
            setPreferenceNotice(null);
            setBrowserAlertNotice(null);
            setSettingsOpen(false);
            setPopoverOpen(false);
            popover.current?.close();
            settings.current?.close();
            releaseDevice();
            known.current = null;
            return;
          }
          await linkBeforeFeed();
          if (!active || sent !== latest || generation.current !== started) return;
          known.current = next.human.id;
          setInbox(next);
          surfaceNew(next, toast, tRef.current);
        })
        .catch((e: unknown) => {
          if (!active || sent !== latest || generation.current !== started || e instanceof StaleEnableError) return;
          if (e instanceof ApiError && (e.status === 401 || e.status === 403 || e.status === 404)) {
            // Signed out, tenancy lost, or this server serves no human
            // threads at all (CAD-1704): clear everything and stop. The next
            // auth epoch retries.
            stop();
            setSettingsOpen(false);
            setPopoverOpen(false);
            popover.current?.close();
            settings.current?.close();
            if (known.current !== null) releaseDevice();
            known.current = null;
            setInbox(null);
            return;
          }
          setNotice("Notifications are unavailable. Your threads still load.");
        });
    };
    void load();
    timer = window.setInterval(load, INBOX_POLL_MS);
    return () => {
      active = false;
      generation.current++;
      stop();
    };
  }, [authEpoch, toast]);

  // The delivery guard needs this device's view, not another tab's read marker.
  useEffect(() => {
    if (deviceId === null || humanId === undefined) return;
    const epoch = generation.current;
    let active = true;
    const isCurrent = () => generation.current === epoch;
    const publish = () => {
      void reportBrowserPresence(activeRoom(), () => active && isCurrent()).catch(() => undefined);
    };
    const release = () => {
      void reportBrowserPresence(null, isCurrent).catch(() => undefined);
    };
    publish();
    // Half the server's 30-second presence lifetime; background tabs send null.
    const timer = window.setInterval(publish, 15000);
    document.addEventListener("visibilitychange", publish);
    window.addEventListener("pagehide", release);
    return () => {
      active = false;
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", publish);
      window.removeEventListener("pagehide", release);
      release();
    };
  }, [authEpoch, deviceId, humanId, href]);

  // Light-dismiss for the non-modal popover: browsers do not emit `cancel` on
  // `show()` dialogs or dismiss them on outside clicks.
  useEffect(() => {
    if (!popoverOpen || settingsOpen) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented || settings.current?.open) return;
      const openModal = document.querySelector("dialog:modal, [aria-modal='true']");
      if (openModal && !popover.current?.contains(openModal)) return;
      event.preventDefault();
      event.stopPropagation();
      popover.current?.close();
      setPopoverOpen(false);
      bell.current?.focus();
    };
    const onPointerDown = (event: PointerEvent | MouseEvent) => {
      if (settings.current?.open) return;
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (popover.current?.contains(target) || bell.current?.contains(target)) return;
      popover.current?.close();
      setPopoverOpen(false);
    };
    document.addEventListener("keydown", onKeyDown);
    document.addEventListener("pointerdown", onPointerDown);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("pointerdown", onPointerDown);
    };
  }, [popoverOpen, settingsOpen]);

  if (inbox === null) return null;
  const browserAlertsAvailable = inbox.push.available && pushSupported();
  const total = inbox.unread_total;
  const groups = inbox.groups.filter((g) => filter === "all" || g.unread > 0);

  const open = () => {
    popover.current?.show();
    setPopoverOpen(true);
    setNotice(null);
  };
  const close = (restoreFocus = true) => {
    popover.current?.close();
    setPopoverOpen(false);
    if (restoreFocus) bell.current?.focus();
  };
  const toggle = () => {
    if (popoverOpen || popover.current?.open) close();
    else open();
  };
  const closeSettings = () => {
    settings.current?.close();
    setSettingsOpen(false);
  };

  const markAll = () => {
    const mine = generation.current;
    void markNotificationsRead()
      .then(async () => {
        if (generation.current !== mine) return;
        const next = await humanNotifications();
        if (generation.current === mine) setInbox(next);
      })
      .catch(() => {
        if (generation.current === mine) setNotice("Could not mark notifications read.");
      });
  };

  const openGroup = (group: NotificationGroup) => {
    close(false);
    navigate(threadHref(group.room));
  };

  const savePrefs = (next: NotificationPreferences) => {
    const mine = generation.current;
    setInbox((cur) => (cur ? { ...cur, preferences: next } : cur));
    void setNotificationPreferences(next).catch(() => {
      if (generation.current === mine) setPreferenceNotice("Could not save notification settings.");
    });
  };

  const toggleMute = (group: NotificationGroup) => {
    const mine = generation.current;
    void setRoomMuted(group.room, !group.muted)
      .then(async () => {
        if (generation.current !== mine) return;
        const next = await humanNotifications();
        if (generation.current === mine) setInbox(next);
      })
      .catch(() => {
        if (generation.current === mine) setNotice("Could not change this thread's mute.");
      });
  };

  const enable = () => {
    if (inbox.push.public_key === null) {
      setBrowserAlertNotice("Browser alerts are not available on this board.");
      return;
    }
    const mine = generation.current;
    const publicKey = inbox.push.public_key;
    // The same verified identity must still be current at every async stage.
    const isCurrent = () => generation.current === mine;
    // The browser may still hold another account's subscription: release it, then subscribe fresh.
    void releaseBrowserAlerts()
      .then(() => enableBrowserAlerts(publicKey, isCurrent))
      .then((msg) => {
        if (!isCurrent()) return;
        setBrowserAlertNotice(msg);
        setDeviceId(localStorage.getItem(DEVICE_KEY));
      })
      .catch((e: unknown) => {
        if (e instanceof StaleEnableError) return;
        if (isCurrent()) setBrowserAlertNotice("Could not turn on browser alerts.");
      });
  };

  const disable = (id: string) => {
    const mine = generation.current;
    void disableBrowserAlerts(id)
      .then((msg) => {
        if (generation.current !== mine) return;
        setBrowserAlertNotice(msg);
        setDeviceId(null);
      })
      .catch(() => {
        if (generation.current === mine) setBrowserAlertNotice("Could not turn off browser alerts.");
      });
  };

  return (
    <div className="notification-anchor">
      <button
        ref={bell}
        type="button"
        className="btn btn-ghost btn-sm icon-button bell-button"
        aria-haspopup="dialog"
        aria-expanded={popoverOpen}
        aria-label={total > 0
          ? t("Open notifications, {count} unread").replace("{count}", String(total))
          : t("Open notifications")}
        onClick={toggle}
      >
        <IconThreads size={16} />
        {total > 0 && <span className="notification-count num" aria-hidden="true">{total}</span>}
      </button>

      <dialog
        ref={popover}
        className="notification-popover"
        aria-labelledby="notification-popover-title"
        onCancel={(event) => {
          event.preventDefault();
          close();
        }}
        onClose={() => setPopoverOpen(false)}
      >
        <div className="notification-heading">
          <h2 id="notification-popover-title">{t("Notifications")}</h2>
          <div className="flex gap-1">
            <Button variant="ghost" size="sm" aria-label={t("Notification settings")} icon={<IconSettings />} onClick={() => { setSettingsOpen(true); settings.current?.showModal(); }} />
            <Button variant="ghost" size="sm" aria-label={t("Close notifications")} icon={<IconClose />} onClick={() => close()} />
          </div>
        </div>
        <div className="notification-tools">
          <div className="filter-row" role="group" aria-label={t("Show")}>
            <button type="button" className={`filter ${filter === "unread" ? "selected" : ""}`} aria-pressed={filter === "unread"} onClick={() => setFilter("unread")}>{t("Unread")} <span className="notification-unread-count">{total}</span></button>
            <button type="button" className={`filter ${filter === "all" ? "selected" : ""}`} aria-pressed={filter === "all"} onClick={() => setFilter("all")}>{t("All")}</button>
          </div>
          <button type="button" className="lnk" disabled={total === 0} onClick={markAll}>{t("Mark all read")}</button>
        </div>
        {notice !== null && <p className="settings-note" role="status">{t(notice)}</p>}
        <div className="notification-list">
          {groups.length === 0 && (
            <div className="notification-empty">
              <span className="notification-empty-icon"><IconCheck size={18} /></span>
              <div>
                <strong>{filter === "unread" ? t("You're all caught up.") : t("No conversations yet.")}</strong>
                <p>{filter === "unread" ? t("No unread messages. Your history is under All.") : t("Direct messages and mentions will appear here.")}</p>
              </div>
            </div>
          )}
          {groups.map((g) => (
            <div key={g.room} className="notification-row">
              <button
                type="button"
                className="notification-copy"
                onClick={() => openGroup(g)}
                aria-label={t("Open {title}, {count} unread").replace("{title}", () => g.title).replace("{count}", String(g.unread))}
              >
                <span className="chip">{g.kind === "dm" ? t("Direct message") : t("Group")}</span>
                <strong>{g.title}</strong>
                <small>{t("{count} unread").replace("{count}", String(g.unread))}{g.muted ? ` · ${t("muted")}` : ""}</small>
              </button>
              <button type="button" className="lnk" onClick={() => toggleMute(g)}>{g.muted ? t("Unmute") : t("Mute")}</button>
            </div>
          ))}
        </div>
      </dialog>

      <dialog
        ref={settings}
        className="notification-popover notification-settings"
        aria-labelledby="notification-settings-title"
        onCancel={(e) => { e.preventDefault(); closeSettings(); }}
        onClose={() => setSettingsOpen(false)}
      >
        <div className="notification-heading">
          <h2 id="notification-settings-title">{t("Notification settings")}</h2>
          <Button variant="ghost" size="sm" aria-label={t("Close notification settings")} icon={<IconClose />} onClick={closeSettings} />
        </div>
        <div className="notification-setting notification-browser-alerts">
          <span>
            <strong>{t("Browser alerts")}</strong>
            {!browserAlertsAvailable && <small>{t("Browser alerts unavailable")}</small>}
          </span>
          {deviceId !== null ? (
            <button type="button" className="lnk" onClick={() => disable(deviceId)}>{t("Turn off browser alerts")}</button>
          ) : browserAlertsAvailable ? (
            <button type="button" className="lnk" onClick={enable}>{t("Enable browser alerts")}</button>
          ) : null}
        </div>
        {browserAlertNotice !== null && <p className="settings-note" role="status">{t(browserAlertNotice)}</p>}
        {settingsOpen && <PreferenceList prefs={inbox.preferences} onChange={savePrefs} />}
        {preferenceNotice !== null && <p className="settings-note" role="status">{t(preferenceNotice)}</p>}
      </dialog>

    </div>
  );
}

/** Approved A preference rows: real labelled controls only. Nothing is shown that does nothing. */
function PreferenceList({ prefs, onChange }: { prefs: NotificationPreferences; onChange: (next: NotificationPreferences) => void }) {
  const t = useTranslate();
  const rows: Array<{ key: "dm" | "mention" | "sound" | "preview"; title: string; hint: string }> = [
    { key: "dm", title: "Direct messages", hint: "Alert me for new direct messages." },
    { key: "mention", title: "Mentions and replies", hint: "Alert me when someone names me." },
    { key: "sound", title: "Sound", hint: "Play the system notification sound." },
    { key: "preview", title: "Message text in alerts", hint: "Off keeps alerts generic." },
  ];
  return (
    <div>
      {rows.map((r) => (
        <label key={r.key} className="notification-setting">
          <span>
            <strong>{t(r.title)}</strong>
            <small>{t(r.hint)}</small>
          </span>
          <input type="checkbox" checked={Boolean(prefs[r.key])} onChange={(e) => onChange({ ...prefs, [r.key]: e.target.checked })} />
        </label>
      ))}
      <label className="notification-setting">
        <span>
          <strong>{t("Quiet hours")}</strong>
          <small>{t("22:00 to 08:00 in your time zone. Unread counts still update.")}</small>
        </span>
        <select className="field" value={prefs.quiet} onChange={(e) => onChange({ ...prefs, quiet: e.target.value === "22-08" ? "22-08" : "off", zone: browserZone() })}>
          <option value="off">{t("Off")}</option>
          <option value="22-08">{t("22:00 to 08:00")}</option>
        </select>
      </label>
    </div>
  );
}

/** The viewer's own IANA zone, sent with every preference save. */
function browserZone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
}

/**
 * Surface only newly observed, eligible unread DM/mention intents. Advance the
 * cursor before interruption preferences so quiet/muted/visible alerts do not
 * reappear later; ordinary group posts never inherit an old mention.
 */
function surfaceNew(next: Inbox, toast: { show: (title: string, room: string) => void }, t: (english: string) => string): void {
  const active = activeRoom();
  for (const g of next.groups) {
    const seen = announced.get(g.room) ?? 0;
    if (g.alert_seq <= seen) continue;
    announced.set(g.room, g.alert_seq);
    const isDm = g.kind === "dm";
    if (g.room === active || g.muted || (!isDm && !g.mention)) continue;
    if (isQuietNow(next.preferences)) continue;
    if (isDm ? !next.preferences.dm : !next.preferences.mention) continue;
    toast.show(t(isDm ? "New direct message in {title}" : "New mention in {title}").replace("{title}", () => g.title), g.room);
  }
}

/** Invalid IANA zones fail closed, matching push delivery's quiet-hours policy. */
function isQuietNow(prefs: NotificationPreferences): boolean {
  if (prefs.quiet !== "22-08") return false;
  try {
    const hour = Number(new Intl.DateTimeFormat("en", {
      timeZone: prefs.zone,
      hour: "numeric",
      hourCycle: "h23",
    }).format(new Date()));
    return !Number.isInteger(hour) || hour < 8 || hour >= 22;
  } catch {
    return true;
  }
}
