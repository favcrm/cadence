import { useEffect, useRef, useState } from "react";
import "./notifications.css";
import Button from "../../ui/Button";
import { IconCheck, IconClose, IconSettings, IconThreads } from "../../ui/icons";
import { navigate, useHref } from "../../lib/useLocation";
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
  const [inbox, setInbox] = useState<Inbox | null>(null);
  const [filter, setFilter] = useState<"unread" | "all">("unread");
  const [notice, setNotice] = useState<string | null>(null);
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
      if (event.storageArea === localStorage && event.key === DEVICE_KEY) setDeviceId(event.newValue);
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
    setSettingsOpen(false);
    popover.current?.close();

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
              setNotice("Could not sync browser alerts. Your threads still load.");
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
            releaseDevice();
            known.current = null;
            return;
          }
          await linkBeforeFeed();
          if (!active || sent !== latest || generation.current !== started) return;
          known.current = next.human.id;
          setInbox(next);
          surfaceNew(next, toast);
        })
        .catch((e: unknown) => {
          if (!active || sent !== latest || generation.current !== started || e instanceof StaleEnableError) return;
          if (e instanceof ApiError && (e.status === 401 || e.status === 403 || e.status === 404)) {
            // Signed out, tenancy lost, or this server serves no human
            // threads at all (CAD-1704): clear everything and stop. The next
            // auth epoch retries.
            stop();
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

  if (inbox === null) return null;
  const total = inbox.unread_total;
  const groups = inbox.groups.filter((g) => filter === "all" || g.unread > 0);

  const open = () => {
    popover.current?.show();
    setNotice(null);
  };
  const close = () => {
    popover.current?.close();
    bell.current?.focus();
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
    popover.current?.close();
    navigate(threadHref(group.room));
  };

  const savePrefs = (next: NotificationPreferences) => {
    const mine = generation.current;
    setInbox((cur) => (cur ? { ...cur, preferences: next } : cur));
    void setNotificationPreferences(next).catch(() => {
      if (generation.current === mine) setNotice("Could not save notification settings.");
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
      setNotice("Browser alerts are not available on this board.");
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
        setNotice(msg);
        setDeviceId(localStorage.getItem(DEVICE_KEY));
      })
      .catch((e: unknown) => {
        if (e instanceof StaleEnableError) return;
        if (isCurrent()) setNotice("Could not turn on browser alerts.");
      });
  };

  const disable = (id: string) => {
    const mine = generation.current;
    void disableBrowserAlerts(id)
      .then((msg) => {
        if (generation.current !== mine) return;
        setNotice(msg);
        setDeviceId(null);
      })
      .catch(() => {
        if (generation.current === mine) setNotice("Could not turn off browser alerts.");
      });
  };

  return (
    <div className="relative">
      <button
        ref={bell}
        type="button"
        className="btn btn-ghost btn-sm icon-button bell-button"
        aria-haspopup="dialog"
        aria-label={total > 0 ? `Open notifications, ${total} unread` : "Open notifications"}
        onClick={open}
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
      >
        <div className="notification-heading">
          <div>
            <span className="kicker">Notifications</span>
            <h2 id="notification-popover-title">Your conversations</h2>
            <p className="muted">{total} unread messages</p>
          </div>
          <div className="flex gap-1">
            <Button variant="ghost" size="sm" aria-label="Notification settings" icon={<IconSettings />} onClick={() => { setSettingsOpen(true); settings.current?.showModal(); }} />
            <Button variant="ghost" size="sm" aria-label="Close notifications" icon={<IconClose />} onClick={close} />
          </div>
        </div>
        <div className="notification-tools">
          <div className="filter-row" role="group" aria-label="Show">
            <button type="button" className={`filter ${filter === "unread" ? "selected" : ""}`} aria-pressed={filter === "unread"} onClick={() => setFilter("unread")}>Unread</button>
            <button type="button" className={`filter ${filter === "all" ? "selected" : ""}`} aria-pressed={filter === "all"} onClick={() => setFilter("all")}>All</button>
          </div>
          <button type="button" className="lnk" disabled={total === 0} onClick={markAll}>Mark all read</button>
        </div>
        {notice !== null && <p className="settings-note" role="status">{notice}</p>}
        <div className="notification-list">
          {groups.length === 0 && (
            <div className="notification-empty">
              <IconCheck size={18} />
              <strong>You're all caught up.</strong>
              <p>No unread messages. Your history is under All.</p>
            </div>
          )}
          {groups.map((g) => (
            <div key={g.room} className="notification-row">
              <button type="button" className="notification-copy" onClick={() => openGroup(g)} aria-label={`Open ${g.title}, ${g.unread} unread`}>
                <span className="chip">{g.kind === "dm" ? "Direct message" : "Group"}</span>
                <strong>{g.title}</strong>
                <small>{g.unread} unread{g.muted ? " · muted" : ""}</small>
              </button>
              <button type="button" className="lnk" onClick={() => toggleMute(g)}>{g.muted ? "Unmute" : "Mute"}</button>
            </div>
          ))}
        </div>
        <div className="notification-footer">
          <span>Notification links never grant access.</span>
          {deviceId !== null && (
            <button type="button" className="lnk" onClick={() => disable(deviceId)}>Turn off browser alerts</button>
          )}
          {inbox.push.available && pushSupported() ? (
            <button type="button" className="lnk" onClick={enable}>Enable browser alerts</button>
          ) : (
            <span>Browser alerts unavailable</span>
          )}
        </div>
      </dialog>

      <dialog ref={settings} className="notification-popover notification-settings" aria-labelledby="notification-settings-title" onCancel={(e) => { e.preventDefault(); settings.current?.close(); }}>
        <div className="notification-heading">
          <h2 id="notification-settings-title">Notification settings</h2>
          <Button variant="ghost" size="sm" aria-label="Close notification settings" icon={<IconClose />} onClick={() => settings.current?.close()} />
        </div>
        {settingsOpen && <PreferenceList prefs={inbox.preferences} onChange={savePrefs} />}
      </dialog>

    </div>
  );
}

/** Approved A preference rows: real labelled controls only. Nothing is shown that does nothing. */
function PreferenceList({ prefs, onChange }: { prefs: NotificationPreferences; onChange: (next: NotificationPreferences) => void }) {
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
            <strong>{r.title}</strong>
            <small>{r.hint}</small>
          </span>
          <input type="checkbox" checked={Boolean(prefs[r.key])} onChange={(e) => onChange({ ...prefs, [r.key]: e.target.checked })} />
        </label>
      ))}
      <label className="notification-setting">
        <span>
          <strong>Quiet hours</strong>
          <small>22:00 to 08:00 in your time zone. Unread counts still update.</small>
        </span>
        <select className="field" value={prefs.quiet} onChange={(e) => onChange({ ...prefs, quiet: e.target.value === "22-08" ? "22-08" : "off", zone: browserZone() })}>
          <option value="off">Off</option>
          <option value="22-08">22:00 to 08:00</option>
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
function surfaceNew(next: Inbox, toast: { show: (title: string, room: string) => void }): void {
  const active = activeRoom();
  for (const g of next.groups) {
    const seen = announced.get(g.room) ?? 0;
    if (g.alert_seq <= seen) continue;
    announced.set(g.room, g.alert_seq);
    const isDm = g.kind === "dm";
    if (g.room === active || g.muted || (!isDm && !g.mention)) continue;
    if (isQuietNow(next.preferences)) continue;
    if (isDm ? !next.preferences.dm : !next.preferences.mention) continue;
    toast.show(`New ${isDm ? "direct message" : "mention"} in ${g.title}`, g.room);
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
