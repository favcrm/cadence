import { useEffect, useRef, useState } from "react";
import "./notifications.css";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import { IconBell, IconCheck, IconClose, IconSettings } from "../../ui/icons";
import { fmtTime } from "../../lib/fmt";
import { onPrivateReadsPaused, onPrivateSessionReset } from "../../lib/privateSession";
import { navigate, useHref } from "../../lib/useLocation";
import { useTranslate } from "../../lib/locale";
import { ApiError } from "../../lib/api";
import type { HostedLocalAssignmentTodo } from "../../lib/types";
import { localAssignmentFingerprint } from "../home/todoLocal";
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
import {
  HUMAN_LIVE_POLL_MS,
  isHumanThreadsStreamLive,
  subscribeHumanThreadsStream,
} from "./humanThreadsStream";
import { useNotificationToast } from "./NotificationToast";
import { setChannelsUnread } from "./channelsUnread";
import { playNotificationChime } from "./notificationSound";

/** The newest eligible unread alert intent already observed per room. */
const announced = new Map<string, number>();

/** Open background-tab desktop notifications keyed by room id. */
const desktopAlerts = new Map<string, Notification>();

const ANNOUNCED_PREFIX = "cadence.notif.announced.v1:";
const ANNOUNCED_CAP = 200;
const DESKTOP_ALERTS_KEY = "cadence.desktop_alerts.v1";

function hashStorageKey(value: string): string {
  let h1 = 0x811c9dc5;
  let h2 = 0x811c9dc5 ^ 0x9e3779b9;
  for (let i = 0; i < value.length; i += 1) {
    const c = value.charCodeAt(i);
    h1 = Math.imul(h1 ^ c, 0x01000193);
    h2 = Math.imul(h2 ^ (c + 1), 0x01000193);
  }
  return `${(h1 >>> 0).toString(16).padStart(8, "0")}${(h2 >>> 0).toString(16).padStart(8, "0")}`;
}

function readStoredAnnounced(humanId: string): Record<string, number> {
  try {
    const raw = localStorage.getItem(`${ANNOUNCED_PREFIX}${hashStorageKey(humanId)}`);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    const out: Record<string, number> = {};
    for (const [k, v] of Object.entries(parsed)) {
      if (typeof v === "number" && Number.isSafeInteger(v) && v > 0) out[k] = v;
    }
    return out;
  } catch {
    return {};
  }
}

function writeStoredAnnounced(humanId: string, record: Record<string, number>): void {
  try {
    const entries = Object.entries(record);
    const bounded = entries.length > ANNOUNCED_CAP ? Object.fromEntries(entries.slice(-ANNOUNCED_CAP)) : record;
    localStorage.setItem(`${ANNOUNCED_PREFIX}${hashStorageKey(humanId)}`, JSON.stringify(bounded));
  } catch {
    // Ignore storage quota or restricted storage errors.
  }
}

function clearStoredAnnounced(humanId?: string | null): void {
  try {
    if (humanId) {
      localStorage.removeItem(`${ANNOUNCED_PREFIX}${hashStorageKey(humanId)}`);
      return;
    }
    const remove: string[] = [];
    for (let i = 0; i < localStorage.length; i += 1) {
      const key = localStorage.key(i);
      if (key && key.startsWith(ANNOUNCED_PREFIX)) remove.push(key);
    }
    for (const key of remove) localStorage.removeItem(key);
  } catch {
    // Ignore storage access errors.
  }
}

function closeDesktopNotifications(room?: string): void {
  if (room !== undefined) {
    const existing = desktopAlerts.get(room);
    if (existing) {
      try { existing.close(); } catch { /* ignore */ }
      desktopAlerts.delete(room);
    }
    return;
  }
  for (const n of desktopAlerts.values()) {
    try { n.close(); } catch { /* ignore */ }
  }
  desktopAlerts.clear();
}

function desktopAlertsSupported(): boolean {
  return (
    typeof window !== "undefined" &&
    "Notification" in window &&
    typeof window.Notification?.requestPermission === "function"
  );
}

function desktopAlertsActive(): boolean {
  if (!desktopAlertsSupported()) return false;
  if (window.Notification.permission !== "granted") return false;
  try {
    return localStorage.getItem(DESKTOP_ALERTS_KEY) === "on";
  } catch {
    return false;
  }
}

function formatSenderPreview(
  sender: string | null | undefined,
  senderKind: "human" | "agent" | null | undefined,
  preview: string | null | undefined,
): string | null {
  const cleanPreview = preview?.trim();
  if (!cleanPreview) return null;
  const cleanSender = sender?.trim();
  if (!cleanSender) return cleanPreview;
  const label = senderKind === "agent" ? `@${cleanSender.replace(/^@+/, "")}` : cleanSender;
  return `${label}: ${cleanPreview}`;
}

onPrivateReadsPaused(() => {
  closeDesktopNotifications();
});

onPrivateSessionReset(() => {
  closeDesktopNotifications();
  clearStoredAnnounced();
  announced.clear();
});

/** Private inbox poll: authorized, private state only. Never shared SSE. */
const INBOX_POLL_MS = 15000;
const BROWSER_SYNC_ERROR = "Could not sync browser alerts. Your threads still load.";

const DEVICE_KEY = "cadence.push.device";
const LOCAL_ASSIGNMENT_ALERTS_PREFIX = "cadence.notifications.local_assignments.v1:";

function readAssignmentAlertKeys(humanId: string | undefined): Set<string> {
  if (!humanId) return new Set();
  try {
    if (typeof localStorage === "undefined") return new Set();
    const raw = localStorage.getItem(`${LOCAL_ASSIGNMENT_ALERTS_PREFIX}${humanId}`);
    if (!raw) return new Set();
    const parsed = JSON.parse(raw) as unknown;
    return Array.isArray(parsed)
      ? new Set(parsed.filter((item): item is string => typeof item === "string"))
      : new Set();
  } catch {
    return new Set();
  }
}

function writeAssignmentAlertKeys(humanId: string | undefined, keys: ReadonlySet<string>): void {
  if (!humanId) return;
  try {
    if (typeof localStorage === "undefined") return;
    localStorage.setItem(
      `${LOCAL_ASSIGNMENT_ALERTS_PREFIX}${humanId}`,
      JSON.stringify([...keys].slice(-64)),
    );
  } catch {
    // Best-effort client persistence only.
  }
}

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
export default function NotificationInbox({
  authEpoch,
  localAssignmentTodos,
  onOpenIssue,
}: {
  authEpoch: string;
  localAssignmentTodos?: readonly HostedLocalAssignmentTodo[];
  onOpenIssue?: (id: string) => void;
}) {
  const t = useTranslate();
  const tRef = useRef(t);
  useEffect(() => {
    tRef.current = t;
  }, [t]);
  const [inbox, setInbox] = useState<Inbox | null>(null);
  const [readAssignmentKeys, setReadAssignmentKeys] = useState<Set<string>>(() => new Set());
  const [filter, setFilter] = useState<"unread" | "all">("unread");
  const [notice, setNotice] = useState<string | null>(null);
  const [preferenceNotice, setPreferenceNotice] = useState<string | null>(null);
  const [browserAlertNotice, setBrowserAlertNotice] = useState<string | null>(null);
  const [popoverOpen, setPopoverOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [deviceId, setDeviceId] = useState<string | null>(() => localStorage.getItem(DEVICE_KEY));
  const [desktopEnabled, setDesktopEnabled] = useState<boolean>(() => desktopAlertsActive());
  const generation = useRef(0);
  const latestPreferenceSave = useRef(0);
  const hydratedAssignments = useRef(false);
  const seenAssignmentKeys = useRef(new Set<string>());
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
      if (event.storageArea !== localStorage) return;
      if (event.key === DEVICE_KEY) {
        setDeviceId(event.newValue);
        setBrowserAlertNotice(null);
      } else if (event.key === DESKTOP_ALERTS_KEY) {
        setDesktopEnabled(desktopAlertsActive());
        setBrowserAlertNotice(null);
      }
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, []);

  // Auto-clear lingering toast cards and desktop notifications once the viewer focuses that thread.
  useEffect(() => {
    const clearVisible = () => {
      const room = activeRoom();
      if (!room) return;
      toast.clear(room);
      closeDesktopNotifications(room);
    };
    clearVisible();
    document.addEventListener("visibilitychange", clearVisible);
    return () => document.removeEventListener("visibilitychange", clearVisible);
  }, [href, toast]);

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
    hydratedAssignments.current = false;
    seenAssignmentKeys.current.clear();
    setReadAssignmentKeys(new Set());
    closeDesktopNotifications();
    toast.clear();
    setChannelsUnread(0);
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
    let pollMs = INBOX_POLL_MS;
    let stopStream: (() => void) | null = null;
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
              setBrowserAlertNotice((notice) => notice === BROWSER_SYNC_ERROR ? null : notice);
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
              setBrowserAlertNotice(BROWSER_SYNC_ERROR);
            }
          })
          .finally(() => {
            linking = null;
          });
      }
      return linking;
    };
    const restartTimer = (nextMs: number) => {
      if (nextMs === pollMs) return;
      window.clearInterval(timer);
      pollMs = nextMs;
      timer = window.setInterval(load, pollMs);
    };
    const stop = () => {
      window.clearInterval(timer);
      stopStream?.();
      stopStream = null;
    };
    const ensureStream = () => {
      if (stopStream !== null || !active) return;
      stopStream = subscribeHumanThreadsStream((msg) => {
        if (!active) return;
        if (msg.kind === "open") {
          restartTimer(HUMAN_LIVE_POLL_MS);
          if (msg.reconnected) load();
          return;
        }
        if (msg.kind === "degraded") {
          restartTimer(INBOX_POLL_MS);
          return;
        }
        if (msg.scopes.includes("notifications")) load();
      });
      if (isHumanThreadsStreamLive()) restartTimer(HUMAN_LIVE_POLL_MS);
    };
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
            hydratedAssignments.current = false;
            seenAssignmentKeys.current.clear();
            setReadAssignmentKeys(new Set());
            clearStoredAnnounced(known.current);
            closeDesktopNotifications();
            toast.clear();
            setChannelsUnread(0);
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
          setChannelsUnread(next.unread_total);
          surfaceNew(next, toast, tRef.current);
          ensureStream();
        })
        .catch((e: unknown) => {
          if (!active || sent !== latest || generation.current !== started || e instanceof StaleEnableError) return;
          if (e instanceof ApiError && (e.status === 401 || e.status === 403 || e.status === 404)) {
            // Signed out, tenancy lost, or this server serves no human
            // threads at all (CAD-1704): clear everything and stop. The next
            // auth epoch retries.
            stop();
            announced.clear();
            clearStoredAnnounced(known.current);
            closeDesktopNotifications();
            toast.clear();
            setChannelsUnread(0);
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
    timer = window.setInterval(load, pollMs);
    return () => {
      active = false;
      generation.current++;
      closeDesktopNotifications();
      setChannelsUnread(0);
      stop();
    };
  }, [authEpoch, toast]);

  useEffect(() => {
    if (!humanId) return;
    setReadAssignmentKeys(readAssignmentAlertKeys(humanId));
  }, [humanId]);

  useEffect(() => {
    if (!inbox || localAssignmentTodos === undefined) return;
    if (!hydratedAssignments.current) {
      hydratedAssignments.current = true;
      for (const item of localAssignmentTodos) {
        seenAssignmentKeys.current.add(localAssignmentFingerprint(item));
      }
      return;
    }
    const storedRead = readAssignmentAlertKeys(inbox.human.id);
    const quiet = isQuietNow(inbox.preferences);
    for (const item of localAssignmentTodos) {
      const key = localAssignmentFingerprint(item);
      if (seenAssignmentKeys.current.has(key)) continue;
      seenAssignmentKeys.current.add(key);
      if (storedRead.has(key) || quiet) continue;
      const label =
        item.assignment.state === "failed"
          ? tRef.current("Failed")
          : item.category === "blocked"
            ? tRef.current("Blocked")
            : tRef.current("Finished");
      toast.show(`${label} · ${item.issue_id}`, `hla:${key}`, {
        href: `/?issue=${encodeURIComponent(item.issue_id)}`,
        note: item.assignment.text?.trim() || item.issue_title,
        translateNote: false,
      });
    }
  }, [inbox, localAssignmentTodos, toast]);

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
  const assignmentAlerts = (localAssignmentTodos ?? []).map((item) => {
    const key = localAssignmentFingerprint(item);
    const chipLabel =
      item.assignment.state === "failed"
        ? t("Failed")
        : item.category === "blocked"
          ? t("Blocked")
          : t("Finished");
    return {
      key,
      issueId: item.issue_id,
      title: `${item.issue_id} · ${item.issue_title}`,
      chipLabel,
      summary: item.assignment.text?.trim() || item.issue_title,
      unread: !readAssignmentKeys.has(key),
    };
  });
  const unreadAssignmentCount = assignmentAlerts.filter((a) => a.unread).length;
  const visibleAssignmentAlerts = assignmentAlerts.filter((a) => filter === "all" || a.unread);
  const total = inbox.unread_total + unreadAssignmentCount;
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
    if (assignmentAlerts.length > 0) {
      const nextRead = new Set(readAssignmentKeys);
      for (const alert of assignmentAlerts) nextRead.add(alert.key);
      writeAssignmentAlertKeys(humanId, nextRead);
      setReadAssignmentKeys(nextRead);
    }
    if (inbox.unread_total === 0) return;
    const mine = generation.current;
    void markNotificationsRead()
      .then(async () => {
        if (generation.current !== mine) return;
        const next = await humanNotifications();
        if (generation.current === mine) {
          setInbox(next);
          setChannelsUnread(next.unread_total);
          for (const g of next.groups) {
            if (g.unread === 0) {
              toast.clear(g.room);
              closeDesktopNotifications(g.room);
            }
          }
          if (next.unread_total === 0) {
            toast.clear();
            closeDesktopNotifications();
          }
        }
      })
      .catch(() => {
        if (generation.current === mine) setNotice("Could not mark notifications read.");
      });
  };

  const openAssignmentAlert = (alert: (typeof assignmentAlerts)[number]) => {
    const nextRead = new Set(readAssignmentKeys);
    nextRead.add(alert.key);
    writeAssignmentAlertKeys(humanId, nextRead);
    setReadAssignmentKeys(nextRead);
    toast.clear(`hla:${alert.key}`);
    close(false);
    if (onOpenIssue) onOpenIssue(alert.issueId);
    else navigate(`/?issue=${encodeURIComponent(alert.issueId)}`);
  };

  const openGroup = (group: NotificationGroup) => {
    close(false);
    navigate(threadHref(group.room));
  };

  const savePrefs = (next: NotificationPreferences) => {
    const mine = generation.current;
    const save = ++latestPreferenceSave.current;
    const isCurrent = () => generation.current === mine && latestPreferenceSave.current === save;
    setInbox((cur) => (cur ? { ...cur, preferences: next } : cur));
    void setNotificationPreferences(next).then(
      () => {
        if (isCurrent()) setPreferenceNotice(null);
      },
      () => {
        if (isCurrent()) setPreferenceNotice("Could not save notification settings.");
      },
    );
  };

  const toggleMute = (group: NotificationGroup) => {
    const mine = generation.current;
    void setRoomMuted(group.room, !group.muted)
      .then(async () => {
        if (generation.current !== mine) return;
        const next = await humanNotifications();
        if (generation.current === mine) {
          setInbox(next);
          setChannelsUnread(next.unread_total);
        }
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

  const enableDesktop = async () => {
    if (!desktopAlertsSupported()) {
      setBrowserAlertNotice("Browser alerts unavailable");
      return;
    }
    const mine = generation.current;
    try {
      const permission =
        window.Notification.permission === "granted"
          ? "granted"
          : await window.Notification.requestPermission();
      if (generation.current !== mine) return;
      if (permission === "granted") {
        try {
          localStorage.setItem(DESKTOP_ALERTS_KEY, "on");
        } catch {
          // ignore storage quota errors
        }
        setDesktopEnabled(true);
        setBrowserAlertNotice("On for open tabs in the background.");
      } else if (permission === "denied") {
        setDesktopEnabled(false);
        setBrowserAlertNotice("Blocked in your browser settings. In-app unread still works.");
      } else {
        setDesktopEnabled(false);
        setBrowserAlertNotice(null);
      }
    } catch {
      if (generation.current === mine) setBrowserAlertNotice("Could not turn on browser alerts.");
    }
  };

  const disableDesktop = () => {
    try {
      localStorage.setItem(DESKTOP_ALERTS_KEY, "off");
    } catch {
      // ignore storage quota errors
    }
    closeDesktopNotifications();
    setDesktopEnabled(false);
    setBrowserAlertNotice(null);
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
        <IconBell size={16} />
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
          <button
            type="button"
            className="lnk"
            disabled={total === 0}
            title={t("Clears the inbox badge without marking channel history read.")}
            onClick={markAll}
          >
            {t("Mark all read")}
          </button>
        </div>
        {notice !== null && <p className="settings-note" role="status">{t(notice)}</p>}
        <div className="notification-list">
          {groups.length === 0 && visibleAssignmentAlerts.length === 0 && (
            <div className="notification-empty">
              <span className="notification-empty-icon"><IconCheck size={18} /></span>
              <div>
                <strong>{filter === "unread" ? t("You're all caught up.") : t("No conversations yet.")}</strong>
                <p>{filter === "unread" ? t("No unread messages. Your history is under All.") : t("Direct messages and mentions will appear here.")}</p>
              </div>
            </div>
          )}
          {visibleAssignmentAlerts.map((alert) => (
            <div key={alert.key} className="notification-row" data-local-assignment-alert={alert.issueId}>
              <button
                type="button"
                className="notification-copy"
                onClick={() => openAssignmentAlert(alert)}
              >
                <span className="chip">{alert.chipLabel}</span>
                <strong>{alert.title}</strong>
                <small>{alert.summary}{alert.unread ? "" : ` · ${t("read")}`}</small>
              </button>
            </div>
          ))}
          {groups.map((g) => {
            const snippet = inbox.preferences.preview
              ? formatSenderPreview(g.latest_sender, g.latest_sender_kind, g.latest_preview)
              : null;
            return (
              <div key={g.room} className="notification-row">
                <button
                  type="button"
                  className="notification-copy"
                  onClick={() => openGroup(g)}
                  aria-label={t("Open {title}, {count} unread").replace("{title}", () => g.title).replace("{count}", String(g.unread))}
                >
                  <div className="notification-meta-row">
                    <span className="chip">{g.kind === "dm" ? t("Direct message") : t("Group channel")}</span>
                    {g.mention && <span className="chip chip-mention">@{t("Mention")}</span>}
                    {g.latest_at ? <time className="notification-time num">{fmtTime(g.latest_at)}</time> : null}
                  </div>
                  <strong>{g.title}</strong>
                  {snippet !== null && <span className="notification-preview">{snippet}</span>}
                  <small>{t("{count} unread").replace("{count}", String(g.unread))}{g.muted ? ` · ${t("muted")}` : ""}</small>
                </button>
                <button type="button" className="lnk" onClick={() => toggleMute(g)}>{g.muted ? t("Unmute") : t("Mute")}</button>
              </div>
            );
          })}
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
            {!browserAlertsAvailable && desktopAlertsSupported() ? (
              <small>
                {window.Notification.permission === "denied"
                  ? t("Blocked in your browser settings. In-app unread still works.")
                  : desktopEnabled
                    ? t("On for open tabs in the background.")
                    : t("Alert me while this tab is in the background.")}
              </small>
            ) : !browserAlertsAvailable ? (
              <small>{t("Browser alerts unavailable")}</small>
            ) : null}
          </span>
          {deviceId !== null ? (
            <button type="button" className="lnk" onClick={() => disable(deviceId)}>{t("Turn off browser alerts")}</button>
          ) : browserAlertsAvailable ? (
            <button type="button" className="lnk" onClick={enable}>{t("Enable browser alerts")}</button>
          ) : desktopAlertsSupported() && window.Notification.permission !== "denied" ? (
            desktopEnabled ? (
              <button type="button" className="lnk" onClick={disableDesktop}>{t("Turn off browser alerts")}</button>
            ) : (
              <button type="button" className="lnk" onClick={() => void enableDesktop()}>{t("Enable browser alerts")}</button>
            )
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
      <div className="notification-setting">
        <span>
          <strong>{t("Quiet hours")}</strong>
          <small>{t("22:00 to 08:00 in your time zone. Unread counts still update.")}</small>
        </span>
        <Select
          size="sm"
          value={prefs.quiet}
          aria-label={t("Quiet hours")}
          onChange={(val) => onChange({ ...prefs, quiet: val === "22-08" ? "22-08" : "off", zone: browserZone() })}
          options={[
            { value: "off", label: t("Off") },
            { value: "22-08", label: t("22:00 to 08:00") },
          ]}
        />
      </div>
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
function surfaceNew(
  next: Inbox,
  toast: { show: (title: string, room: string, target?: { href: string; note: string; translateNote?: boolean }) => void },
  t: (english: string) => string,
): void {
  const active = activeRoom();
  const stored = readStoredAnnounced(next.human.id);
  let dirty = false;
  let playSound = false;
  for (const g of next.groups) {
    const hash = hashStorageKey(g.room);
    const seen = Math.max(announced.get(g.room) ?? 0, stored[hash] ?? 0);
    if (g.alert_seq <= seen) {
      if ((announced.get(g.room) ?? 0) < seen) announced.set(g.room, seen);
      continue;
    }
    announced.set(g.room, g.alert_seq);
    delete stored[hash];
    stored[hash] = g.alert_seq;
    dirty = true;
    const isDm = g.kind === "dm";
    if (g.room === active || g.muted || (!isDm && !g.mention)) continue;
    if (isQuietNow(next.preferences)) continue;
    if (isDm ? !next.preferences.dm : !next.preferences.mention) continue;
    const title = t(isDm ? "New direct message in {title}" : "New mention in {title}").replace("{title}", () => g.title);
    const snippet = next.preferences.preview
      ? formatSenderPreview(g.latest_sender, g.latest_sender_kind, g.latest_preview)
      : null;
    if (snippet !== null) {
      toast.show(title, g.room, { href: threadHref(g.room), note: snippet, translateNote: false });
    } else {
      toast.show(title, g.room);
    }
    if (next.preferences.sound) playSound = true;
    if (!next.push.available && desktopAlertsActive() && typeof document !== "undefined" && document.visibilityState !== "visible") {
      try {
        closeDesktopNotifications(g.room);
        const desktop = new window.Notification(title, {
          body: snippet ?? t("Unread channel messages"),
          tag: `cadence-thread-${hash}`,
        });
        desktop.onclick = () => {
          try { window.focus(); } catch { /* ignore */ }
          navigate(threadHref(g.room));
          closeDesktopNotifications(g.room);
        };
        desktop.onclose = () => {
          if (desktopAlerts.get(g.room) === desktop) desktopAlerts.delete(g.room);
        };
        desktopAlerts.set(g.room, desktop);
      } catch {
        // Ignore restricted desktop Notification errors.
      }
    }
  }
  if (dirty) writeStoredAnnounced(next.human.id, stored);
  if (playSound) playNotificationChime();
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
