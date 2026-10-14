import { enrollPush, rebindPush, unenrollPush } from "./humanThreadsClient";

/**
 * CAD-1423: explicit browser opt-in for Web Push. Runs only from a user gesture
 * (the inbox "Enable browser alerts" button), never on page load. Requests the
 * real permission, registers the push/click service worker, and subscribes with
 * the platform's public application-server key. Unsupported or denied states return a message
 * and leave in-app unread working; nothing is retried automatically.
 */

export const SW_PATH = "/notifications-sw.js";

function base64urlToBytes(value: string): Uint8Array<ArrayBuffer> {
  const pad = "=".repeat((4 - (value.length % 4)) % 4);
  const b64 = (value + pad).replace(/-/g, "+").replace(/_/g, "/");
  const raw = atob(b64);
  const bytes = new Uint8Array(new ArrayBuffer(raw.length));
  for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
  return bytes;
}

function bytesToBase64url(buf: ArrayBuffer | null): string {
  if (buf === null) return "";
  let s = "";
  for (const b of new Uint8Array(buf)) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function pushSupported(): boolean {
  return (
    window.isSecureContext &&
    "serviceWorker" in navigator &&
    "PushManager" in window &&
    "Notification" in window
  );
}

const DEVICE_KEY = "cadence.push.device";

/** Thrown when the auth epoch moved on while an enable was in flight. */
export class StaleEnableError extends Error {}

/**
 * Serialize release, enable and disable so a stale enable can never finish
 * after a release for a different identity has started.
 */
let pushQueue: Promise<unknown> = Promise.resolve();
function serialized<T>(task: () => Promise<T>): Promise<T> {
  const run = pushQueue.then(task, task);
  pushQueue = run.catch(() => undefined);
  return run;
}

/**
 * Link this epoch to the browser's existing subscription without prompting,
 * registering a worker, or creating consent. A concealed server no-match
 * closes the local subscription; transport failures leave it untouched.
 */
export function rebindExistingBrowserAlerts(isCurrent: () => boolean): Promise<string | null> {
  return serialized(async () => {
    if (!pushSupported()) return null;
    const reg = await navigator.serviceWorker.getRegistration(SW_PATH);
    if (!isCurrent()) throw new StaleEnableError();
    const sub = await reg?.pushManager.getSubscription();
    if (!isCurrent()) throw new StaleEnableError();
    if (!reg || !sub) {
      localStorage.removeItem(DEVICE_KEY);
      return null;
    }
    const { device_id } = await rebindPush(sub.endpoint);
    if (!isCurrent()) throw new StaleEnableError();
    if (device_id === null) {
      const notifications = await reg.getNotifications();
      if (!isCurrent()) throw new StaleEnableError();
      notifications.forEach((notification) => notification.close());
      await sub.unsubscribe();
      if (!isCurrent()) throw new StaleEnableError();
      localStorage.removeItem(DEVICE_KEY);
      return null;
    }
    localStorage.setItem(DEVICE_KEY, device_id);
    return device_id;
  });
}

/**
 * Enroll this browser after an explicit user gesture. Stale completions release
 * only the subscription and consent this operation just created.
 */
export function enableBrowserAlerts(publicKey: string, isCurrent: () => boolean): Promise<string> {
  return serialized(async () => {
    if (!pushSupported()) return "Browser alerts are not available in this browser. In-app unread still works.";
    const permission = await Notification.requestPermission();
    if (!isCurrent()) throw new StaleEnableError();
    if (permission !== "granted") return "Browser alerts are off. In-app unread still works.";
    await navigator.serviceWorker.register(SW_PATH, { scope: "/" });
    // register() resolves before activation; subscribe needs the active worker (InvalidStateError on a first visit).
    const reg = await navigator.serviceWorker.ready;
    if (!isCurrent()) throw new StaleEnableError();
    const existing = await reg.pushManager.getSubscription();
    const sub =
      existing ??
      (await reg.pushManager.subscribe({
        userVisibleOnly: true,
        applicationServerKey: base64urlToBytes(publicKey),
      }));
    if (!isCurrent()) {
      if (!existing) await sub.unsubscribe().catch(() => undefined);
      throw new StaleEnableError();
    }
    const keys = {
      p256dh: bytesToBase64url(sub.getKey("p256dh")),
      auth: bytesToBase64url(sub.getKey("auth")),
    };
    const { device_id } = await enrollPush({ endpoint: sub.endpoint, keys });
    if (!isCurrent()) {
      // The server enrolled this device for the old identity; take it back.
      await unenrollPush(device_id).catch(() => undefined);
      await sub.unsubscribe().catch(() => undefined);
      throw new StaleEnableError();
    }
    localStorage.setItem(DEVICE_KEY, device_id);
    return "Browser alerts are on for this device.";
  });
}

/**
 * Local release for an identity change, sign-out or a fresh enrollment: close
 * this browser's own alerts, drop its push subscription, and ask the server to
 * remove the device it enrolled. The server only removes a device the current
 * human owns, so another account's record is never touched. After sign-out the
 * server call is refused; the row then sends nothing because its session is gone.
 */
export function releaseBrowserAlerts(): Promise<void> {
  return serialized(async () => {
    const id = localStorage.getItem(DEVICE_KEY);
    if (pushSupported()) {
      const reg = await navigator.serviceWorker.getRegistration(SW_PATH);
      if (reg) {
        (await reg.getNotifications()).forEach((n) => n.close());
        const sub = await reg.pushManager.getSubscription();
        await sub?.unsubscribe();
      }
    }
    if (id !== null) await unenrollPush(id).catch(() => undefined);
    localStorage.removeItem(DEVICE_KEY);
  });
}

/** Remove this browser's subscription and its server enrollment. */
export function disableBrowserAlerts(deviceId: string): Promise<string> {
  return serialized(async () => {
    if (pushSupported()) {
      const reg = await navigator.serviceWorker.getRegistration(SW_PATH);
      const sub = await reg?.pushManager.getSubscription();
      await sub?.unsubscribe();
    }
    const { removed } = await unenrollPush(deviceId);
    localStorage.removeItem(DEVICE_KEY);
    return removed ? "Browser alerts are off for this device." : "This device was not enrolled.";
  });
}
