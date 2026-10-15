import { enrollPush, rebindPush, setPushPresence, unenrollPush } from "./humanThreadsClient";
import { beginPrivateOperation, privateReadsAllowed, type PrivateOperation } from "../../lib/privateSession";

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
let presenceTab: string | undefined;

/** Thrown when this attempt's private session or caller epoch is no longer current. */
export class StaleEnableError extends Error {}

/**
 * Serialize release, enable and disable so a stale enable can never finish
 * after a release for a different identity has started. The private operation
 * is taken before the queue wait, so a pause stays retired after readmission.
 */
let pushQueue: Promise<unknown> = Promise.resolve();
function serialized<T>(task: () => Promise<T>): Promise<T> {
  const run = pushQueue.then(task, task);
  pushQueue = run.catch(() => undefined);
  return run;
}

/** False once this operation was paused or reset, including after the same identity is readmitted. */
function sessionCurrent(operation: PrivateOperation | null): boolean {
  if (operation === null) return false;
  try {
    operation.assertCurrent();
    return true;
  } catch {
    return false;
  }
}

async function dropCreated(created: boolean, sub: PushSubscription): Promise<void> {
  if (!created) return;
  await sub.unsubscribe().catch(() => undefined);
}

// Capture before the queue; denied entry permits local release but never a later server call.
function runOwned<T>(task: (operation: PrivateOperation | null) => Promise<T>): Promise<T> {
  const operation = privateReadsAllowed() ? beginPrivateOperation() : null;
  return serialized(async () => {
    try {
      return await task(operation);
    } finally {
      operation?.dispose();
    }
  });
}

/** Order presence with enrollment/release so a stale view cannot suppress a newly bound device. */
export function reportBrowserPresence(room: string | null, isCurrent: () => boolean): Promise<void> {
  return runOwned(async (operation) => {
    if (!sessionCurrent(operation) || !pushSupported() || !isCurrent()) return;
    const deviceId = localStorage.getItem(DEVICE_KEY);
    if (deviceId === null || !sessionCurrent(operation) || !isCurrent()) return;
    await setPushPresence(deviceId, presenceTab ??= crypto.randomUUID(), room);
  });
}

/**
 * Link this epoch to the browser's existing subscription without prompting,
 * registering a worker, or creating consent. A concealed server no-match
 * closes the local subscription; transport failures leave it untouched.
 */
export function rebindExistingBrowserAlerts(isCurrent: () => boolean): Promise<string | null> {
  return runOwned(async (operation) => {
    if (!sessionCurrent(operation) || !isCurrent()) throw new StaleEnableError();
    if (!pushSupported()) return null;
    const reg = await navigator.serviceWorker.getRegistration(SW_PATH);
    if (!sessionCurrent(operation) || !isCurrent()) throw new StaleEnableError();
    const sub = await reg?.pushManager.getSubscription();
    if (!sessionCurrent(operation) || !isCurrent()) throw new StaleEnableError();
    if (!reg || !sub) {
      localStorage.removeItem(DEVICE_KEY);
      return null;
    }
    const { device_id } = await rebindPush(sub.endpoint);
    if (!sessionCurrent(operation) || !isCurrent()) throw new StaleEnableError();
    if (device_id === null) {
      const notifications = await reg.getNotifications();
      if (!sessionCurrent(operation) || !isCurrent()) throw new StaleEnableError();
      notifications.forEach((notification) => notification.close());
      await sub.unsubscribe();
      if (!sessionCurrent(operation) || !isCurrent()) throw new StaleEnableError();
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
  return runOwned(async (operation) => {
    if (!sessionCurrent(operation) || !isCurrent()) throw new StaleEnableError();
    if (!pushSupported()) return "Browser alerts are not available in this browser. In-app unread still works.";
    const permission = await Notification.requestPermission();
    if (!sessionCurrent(operation) || !isCurrent()) throw new StaleEnableError();
    if (permission !== "granted") return "Browser alerts are off. In-app unread still works.";
    await navigator.serviceWorker.register(SW_PATH, { scope: "/" });
    if (!sessionCurrent(operation) || !isCurrent()) throw new StaleEnableError();
    // register() resolves before activation; subscribe needs the active worker (InvalidStateError on a first visit).
    const reg = await navigator.serviceWorker.ready;
    if (!sessionCurrent(operation) || !isCurrent()) throw new StaleEnableError();
    const existing = await reg.pushManager.getSubscription();
    if (!sessionCurrent(operation) || !isCurrent()) throw new StaleEnableError();
    const sub =
      existing ??
      (await reg.pushManager.subscribe({
        userVisibleOnly: true,
        applicationServerKey: base64urlToBytes(publicKey),
      }));
    const created = existing === null;
    if (!sessionCurrent(operation) || !isCurrent()) {
      await dropCreated(created, sub);
      throw new StaleEnableError();
    }
    const keys = {
      p256dh: bytesToBase64url(sub.getKey("p256dh")),
      auth: bytesToBase64url(sub.getKey("auth")),
    };
    const { device_id } = await enrollPush({ endpoint: sub.endpoint, keys });
    const callerCurrent = isCurrent();
    const originCurrent = sessionCurrent(operation);
    if (!callerCurrent || !originCurrent) {
      if (originCurrent) await unenrollPush(device_id).catch(() => undefined);
      await dropCreated(created, sub);
      throw new StaleEnableError();
    }
    localStorage.setItem(DEVICE_KEY, device_id);
    return "Browser alerts are on for this device.";
  });
}

/**
 * Local release for an identity change, sign-out or a fresh enrollment: close
 * this browser's own alerts and drop its push subscription. When the private
 * operation captured at entry is still current, also ask the server to remove
 * the device it enrolled. The server only removes a device the current human
 * owns, so another account's record is never touched. A release that starts
 * after reads are already denied, or that was paused, still clears this browser
 * and does not mint a delete after readmission.
 */
export function releaseBrowserAlerts(): Promise<void> {
  return runOwned(async (operation) => {
    const id = localStorage.getItem(DEVICE_KEY);
    if (pushSupported()) {
      const reg = await navigator.serviceWorker.getRegistration(SW_PATH);
      if (reg) {
        (await reg.getNotifications()).forEach((n) => n.close());
        const sub = await reg.pushManager.getSubscription();
        await sub?.unsubscribe();
      }
    }
    if (id !== null && sessionCurrent(operation)) await unenrollPush(id).catch(() => undefined);
    localStorage.removeItem(DEVICE_KEY);
  });
}

/** Remove this browser's subscription and its server enrollment. */
export function disableBrowserAlerts(deviceId: string): Promise<string> {
  return runOwned(async (operation) => {
    if (!sessionCurrent(operation)) throw new StaleEnableError();
    if (pushSupported()) {
      const reg = await navigator.serviceWorker.getRegistration(SW_PATH);
      if (!sessionCurrent(operation)) throw new StaleEnableError();
      const sub = await reg?.pushManager.getSubscription();
      if (!sessionCurrent(operation)) throw new StaleEnableError();
      await sub?.unsubscribe();
      if (!sessionCurrent(operation)) throw new StaleEnableError();
    }
    const { removed } = await unenrollPush(deviceId);
    if (!sessionCurrent(operation)) throw new StaleEnableError();
    localStorage.removeItem(DEVICE_KEY);
    return removed ? "Browser alerts are off for this device." : "This device was not enrolled.";
  });
}
