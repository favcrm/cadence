import type * as PrivateSessionModule from "../src/lib/privateSession";
import type * as PushOptInModule from "../src/features/threads/pushOptIn";
import type * as SessionKeyModule from "../src/lib/sessionKey";

/**
 * Browser-alert work has to keep the private lifetime that started it.
 * A pause and same-identity readmission must not let that old queued or
 * browser-held work prompt, subscribe, enroll, unenroll, publish presence,
 * or store a device id. A release that began while reads were already
 * denied still clears this browser locally and must not send a fresh DELETE.
 */
declare function require(name: string): unknown;

type PushCall = {
  method: string;
  path: string;
  credential: string | null;
};

type Outcome<T> = { ok: true; value: T } | { ok: false; error: unknown };

type Gate<T> = {
  promise: Promise<T>;
  finish: (value: T) => void;
  release: () => void;
};

type Arm = "registration" | "subscription" | "unsubscribe" | "permission";

type FakeNotification = { close: () => void };

type FakeSubscription = {
  endpoint: string;
  getKey: (name: PushEncryptionKeyName) => ArrayBuffer | null;
  unsubscribe: () => Promise<boolean>;
};

type FakeRegistration = {
  pushManager: {
    getSubscription: () => Promise<FakeSubscription | null>;
    subscribe: () => Promise<FakeSubscription>;
  };
  getNotifications: () => Promise<FakeNotification[]>;
};

const ENROLL_PATH = "/api/human-threads/push/subscriptions";
const REBIND_PATH = "/api/human-threads/push/subscriptions/rebind";
const PRESENCE_PATH = "/api/human-threads/push/presence";
const DEVICE_KEY = "cadence.push.device";
const PUBLIC_KEY = "AAAAAAAAAAAAAAAAAAAAAAAAAAA";
const ENDPOINT = "https://push.example/subscription";

class MemoryStorage {
  private readonly items = new Map<string, string>();

  get length(): number {
    return this.items.size;
  }

  clear(): void {
    this.items.clear();
  }

  getItem(key: string): string | null {
    return this.items.get(key) ?? null;
  }

  key(index: number): string | null {
    return [...this.items.keys()][index] ?? null;
  }

  removeItem(key: string): void {
    this.items.delete(key);
  }

  setItem(key: string, value: string): void {
    this.items.set(key, String(value));
  }
}

const localStore = new MemoryStorage();
const sessionStore = new MemoryStorage();
const calls: PushCall[] = [];
const failures: string[] = [];

let arm: Arm | null = null;
let scriptPath = "/notifications-sw.js";
let permissionCalls = 0;
let subscribeCalls = 0;
let unsubscribeCalls = 0;
let notificationCloses = 0;
let permissionResult: NotificationPermission = "granted";
let retireOnEnroll = false;
let retireArmed = false;
let rebindDeviceId: string | null = "dev-rebound";
let currentSubscription: FakeSubscription | null = null;
let permissionGate: Gate<NotificationPermission> | null = null;
let registrationGate: Gate<FakeRegistration | undefined> | null = null;
let subscriptionGate: Gate<FakeSubscription | null> | null = null;
let unsubscribeGate: Gate<boolean> | null = null;
let tail: Promise<void> = Promise.resolve();

function createGate<T>(fallback: T): Gate<T> {
  let settled = false;
  let resolveGate: (value: T) => void = () => {};
  const promise = new Promise<T>((resolve) => {
    resolveGate = resolve;
  });
  const settle = (value: T) => {
    if (settled) return;
    settled = true;
    resolveGate(value);
  };
  return { promise, finish: settle, release: () => settle(fallback) };
}

const p256dhKey = Uint8Array.of(1, 2, 3, 4).buffer;
const authKey = Uint8Array.of(5, 6, 7, 8).buffer;
const subscription: FakeSubscription = {
  endpoint: ENDPOINT,
  getKey(name) {
    if (name === "p256dh") return p256dhKey;
    if (name === "auth") return authKey;
    return null;
  },
  unsubscribe() {
    unsubscribeCalls += 1;
    if (arm === "unsubscribe") {
      arm = null;
      const gate = createGate(true);
      unsubscribeGate = gate;
      return gate.promise;
    }
    return Promise.resolve(true);
  },
};

const registration: FakeRegistration = {
  pushManager: {
    getSubscription() {
      if (arm === "subscription") {
        arm = null;
        const gate = createGate<FakeSubscription | null>(currentSubscription);
        subscriptionGate = gate;
        return gate.promise;
      }
      return Promise.resolve(currentSubscription);
    },
    subscribe() {
      subscribeCalls += 1;
      currentSubscription = subscription;
      return Promise.resolve(subscription);
    },
  },
  getNotifications() {
    return Promise.resolve([
      {
        close() {
          notificationCloses += 1;
        },
      },
    ]);
  },
};

class BrowserNotification {
  static permission: NotificationPermission = "default";

  static requestPermission(): Promise<NotificationPermission> {
    permissionCalls += 1;
    if (arm === "permission") {
      arm = null;
      const gate = createGate<NotificationPermission>(permissionResult);
      permissionGate = gate;
      return gate.promise;
    }
    return Promise.resolve(permissionResult);
  }
}

const serviceWorker = {
  getRegistration(scopeURL?: string): Promise<FakeRegistration | undefined> {
    if (scopeURL !== scriptPath) return Promise.resolve(undefined);
    if (arm === "registration") {
      arm = null;
      const gate = createGate<FakeRegistration | undefined>(registration);
      registrationGate = gate;
      return gate.promise;
    }
    return Promise.resolve(registration);
  },
  register(): Promise<FakeRegistration> {
    return Promise.resolve(registration);
  },
  get ready(): Promise<FakeRegistration> {
    return Promise.resolve(registration);
  },
};

function requestPath(input: RequestInfo | URL): string {
  if (typeof input === "string") return new URL(input, "https://cadence.example").pathname;
  if (input instanceof URL) return input.pathname;
  return new URL(input.url, "https://cadence.example").pathname;
}

function json(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function installBrowser(): void {
  const browserWindow = {
    isSecureContext: true,
    PushManager: function PushManager() {},
    Notification: BrowserNotification,
    dispatchEvent(_event: Event): boolean {
      return true;
    },
    location: { origin: "https://cadence.example" },
  };
  Object.defineProperty(globalThis, "window", { value: browserWindow, configurable: true, writable: true });
  Object.defineProperty(globalThis, "navigator", {
    value: { serviceWorker },
    configurable: true,
    writable: true,
  });
  Object.defineProperty(globalThis, "Notification", {
    value: BrowserNotification,
    configurable: true,
    writable: true,
  });
  Object.defineProperty(globalThis, "localStorage", { value: localStore, configurable: true, writable: true });
  Object.defineProperty(globalThis, "sessionStorage", { value: sessionStore, configurable: true, writable: true });
  const fakeFetch: typeof fetch = (input, init) => {
    const path = requestPath(input);
    const method = (init?.method ?? "GET").toUpperCase();
    const headers = new Headers(init?.headers);
    calls.push({
      method,
      path,
      credential: headers.get("X-Cadence-Session"),
    });
    if (retireOnEnroll && method === "POST" && path === ENROLL_PATH) retireArmed = true;
    if (method === "POST" && path === ENROLL_PATH) return Promise.resolve(json({ device_id: "dev-enrolled" }));
    if (method === "POST" && path === REBIND_PATH) return Promise.resolve(json({ device_id: rebindDeviceId }));
    if (method === "POST" && path === PRESENCE_PATH) return Promise.resolve(json({ accepted: true }));
    if (method === "DELETE" && path.startsWith(`${ENROLL_PATH}/`)) return Promise.resolve(json({ removed: true }));
    return Promise.resolve(json({ error: "unexpected" }, 404));
  };
  globalThis.fetch = fakeFetch;
}

installBrowser();

const {
  SW_PATH,
  StaleEnableError,
  disableBrowserAlerts,
  enableBrowserAlerts,
  pushSupported,
  rebindExistingBrowserAlerts,
  releaseBrowserAlerts,
  reportBrowserPresence,
} = require("../src/features/threads/pushOptIn") as typeof PushOptInModule;
const {
  allowPrivateReads,
  pausePrivateReads,
  privateReadsAllowed,
} = require("../src/lib/privateSession") as typeof PrivateSessionModule;
const { setSessionKey } = require("../src/lib/sessionKey") as typeof SessionKeyModule;
scriptPath = SW_PATH;

function check(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

function equal(actual: unknown, expected: unknown, message: string): void {
  if (actual !== expected) throw new Error(`${message}: expected ${String(expected)}, got ${String(actual)}`);
}

function microturn(): Promise<void> {
  return new Promise((resolve) => {
    queueMicrotask(() => resolve());
  });
}

async function until(label: string, ready: () => boolean): Promise<void> {
  for (let attempt = 0; attempt < 40 && !ready(); attempt += 1) await microturn();
  if (!ready()) throw new Error(`${label} did not happen`);
}

function watch<T>(promise: Promise<T>): Promise<T> {
  const settled = promise.then(() => undefined, () => undefined);
  tail = tail.then(() => settled);
  return promise;
}

async function outcome<T>(promise: Promise<T>): Promise<Outcome<T>> {
  try {
    return { ok: true, value: await promise };
  } catch (error) {
    return { ok: false, error };
  }
}

function described(callsFound: PushCall[]): string {
  return callsFound.map((call) => `${call.method} ${call.path} cred=${call.credential ?? ""}`).join("; ");
}

function matching(method: string, path: string): PushCall[] {
  return calls.filter((call) => call.method === method && call.path === path);
}

function unenrollCalls(): PushCall[] {
  return calls.filter((call) => call.method === "DELETE" && call.path.startsWith(`${ENROLL_PATH}/`));
}

function assertNo(found: PushCall[], message: string): void {
  if (found.length !== 0) throw new Error(`${message}: ${described(found)}`);
}

function assertStale(result: Outcome<unknown>, message: string): void {
  if (result.ok) throw new Error(`${message}: expected rejection, fulfilled with ${String(result.value)}`);
  if (!(result.error instanceof StaleEnableError)) {
    const detail = result.error instanceof Error ? `${result.error.name}: ${result.error.message}` : String(result.error);
    throw new Error(`${message}: expected StaleEnableError, got ${detail}`);
  }
}

function assertFulfilled<T>(result: Outcome<T>, message: string): T {
  if (!result.ok) {
    const detail = result.error instanceof Error ? `${result.error.name}: ${result.error.message}` : String(result.error);
    throw new Error(`${message}: expected fulfillment, rejected with ${detail}`);
  }
  return result.value;
}

function releaseAllGates(): void {
  permissionGate?.release();
  registrationGate?.release();
  subscriptionGate?.release();
  unsubscribeGate?.release();
  permissionGate = null;
  registrationGate = null;
  subscriptionGate = null;
  unsubscribeGate = null;
  arm = null;
}

function beginScenario(): void {
  calls.length = 0;
  localStore.clear();
  sessionStore.clear();
  arm = null;
  permissionCalls = 0;
  subscribeCalls = 0;
  unsubscribeCalls = 0;
  notificationCloses = 0;
  permissionResult = "granted";
  retireOnEnroll = false;
  retireArmed = false;
  rebindDeviceId = "dev-rebound";
  currentSubscription = null;
  allowPrivateReads();
  setSessionKey("cred-current");
}

function readmitSameIdentity(nextCredential: string): void {
  pausePrivateReads();
  setSessionKey(nextCredential);
  allowPrivateReads();
  check(privateReadsAllowed(), "readmission allows private reads");
}

async function scenario(name: string, body: () => Promise<void>): Promise<void> {
  beginScenario();
  try {
    await body();
  } catch (error) {
    failures.push(`${name}: ${error instanceof Error ? error.message : String(error)}`);
  } finally {
    releaseAllGates();
    await tail;
    await microturn();
    allowPrivateReads();
  }
}

async function disableHeldAcrossReadmission(): Promise<void> {
  localStorage.setItem(DEVICE_KEY, "dev-1");
  currentSubscription = subscription;
  arm = "unsubscribe";
  const task = watch(disableBrowserAlerts("dev-1"));
  await until("disable waits on unsubscribe", () => unsubscribeGate !== null);
  readmitSameIdentity("cred-readmitted");
  check(unsubscribeGate, "unsubscribe gate");
  unsubscribeGate.finish(true);
  const result = await outcome(task);
  assertNo(unenrollCalls(), "paused disable must not DELETE after same-identity readmission");
  equal(localStorage.getItem(DEVICE_KEY), "dev-1", "paused disable must not clear the device id as a success");
  assertStale(result, "paused disable");
}

async function releaseHeldAcrossReadmission(): Promise<void> {
  localStorage.setItem(DEVICE_KEY, "dev-release");
  currentSubscription = subscription;
  arm = "registration";
  const task = watch(releaseBrowserAlerts());
  await until("release waits on service worker registration", () => registrationGate !== null);
  readmitSameIdentity("cred-readmitted");
  check(registrationGate, "registration gate");
  registrationGate.finish(registration);
  const result = await outcome(task);
  assertNo(unenrollCalls(), "paused release must not DELETE after same-identity readmission");
  check(result.ok, "paused release still finishes local cleanup");
  equal(localStorage.getItem(DEVICE_KEY), null, "paused release still clears the local device id");
  check(unsubscribeCalls >= 1, "paused release still drops the local subscription");
  check(notificationCloses >= 1, "paused release still closes visible notifications");
}

async function releaseSignedOutThenReadmitted(): Promise<void> {
  pausePrivateReads();
  localStorage.setItem(DEVICE_KEY, "dev-signed-out");
  currentSubscription = subscription;
  arm = "registration";
  const task = watch(releaseBrowserAlerts());
  await until("signed-out release waits on registration", () => registrationGate !== null);
  readmitSameIdentity("cred-readmitted");
  check(registrationGate, "registration gate");
  registrationGate.finish(registration);
  const result = await outcome(task);
  assertNo(unenrollCalls(), "signed-out release must not mint a DELETE after readmission");
  check(result.ok, "signed-out release fulfills after local cleanup");
  equal(localStorage.getItem(DEVICE_KEY), null, "signed-out release clears the local device id");
  check(unsubscribeCalls >= 1, "signed-out release drops the local subscription");
  check(notificationCloses >= 1, "signed-out release closes visible notifications");
}

async function queuedEnableAcrossReadmission(): Promise<void> {
  localStorage.setItem(DEVICE_KEY, "dev-release");
  currentSubscription = subscription;
  arm = "registration";
  const releaseTask = watch(releaseBrowserAlerts());
  await until("queue holder waits on registration", () => registrationGate !== null);
  const enableTask = watch(enableBrowserAlerts(PUBLIC_KEY, () => true));
  readmitSameIdentity("cred-readmitted");
  check(registrationGate, "registration gate");
  registrationGate.finish(registration);
  await outcome(releaseTask);
  const enabled = await outcome(enableTask);
  equal(permissionCalls, 0, "queued enable must not request notification permission after readmission");
  equal(subscribeCalls, 0, "queued enable must not subscribe after readmission");
  assertNo(matching("POST", ENROLL_PATH), "queued enable must not enroll after readmission");
  equal(localStorage.getItem(DEVICE_KEY), null, "queued enable must not adopt a device id");
  assertStale(enabled, "queued enable");

  calls.length = 0;
  permissionCalls = 0;
  subscribeCalls = 0;
  currentSubscription = null;
  const fresh = await outcome(watch(enableBrowserAlerts(PUBLIC_KEY, () => true)));
  assertFulfilled(fresh, "fresh enable after a retired queue");
  equal(permissionCalls, 1, "fresh enable requests permission once");
  equal(subscribeCalls, 1, "fresh enable creates one subscription");
  equal(matching("POST", ENROLL_PATH).length, 1, "fresh enable enrolls once");
  equal(localStorage.getItem(DEVICE_KEY), "dev-enrolled", "fresh enable stores the device id returned for this gesture");
}

async function enableHeldAtPermission(): Promise<void> {
  arm = "permission";
  const task = watch(enableBrowserAlerts(PUBLIC_KEY, () => true));
  await until("enable waits on the permission prompt", () => permissionGate !== null);
  equal(permissionCalls, 1, "the prompt started before the pause");
  readmitSameIdentity("cred-readmitted");
  check(permissionGate, "permission gate");
  permissionGate.finish("granted");
  const result = await outcome(task);
  equal(subscribeCalls, 0, "permission that settles after readmission must not subscribe");
  assertNo(matching("POST", ENROLL_PATH), "permission that settles after readmission must not enroll");
  equal(localStorage.getItem(DEVICE_KEY), null, "permission that settles after readmission must not adopt a device id");
  equal(permissionCalls, 1, "readmission must not request permission again");
  assertStale(result, "held enable");
}

async function enableWhileDenied(): Promise<void> {
  pausePrivateReads();
  const result = await outcome(watch(enableBrowserAlerts(PUBLIC_KEY, () => true)));
  equal(permissionCalls, 0, "enable started while denied must not request permission");
  equal(subscribeCalls, 0, "enable started while denied must not subscribe");
  assertNo(matching("POST", ENROLL_PATH), "enable started while denied must not enroll");
  assertStale(result, "denied enable");
}

async function queuedRebindAcrossReadmission(): Promise<void> {
  currentSubscription = subscription;
  localStorage.setItem(DEVICE_KEY, "dev-before");
  arm = "registration";
  const releaseTask = watch(releaseBrowserAlerts());
  await until("queue holder waits on registration", () => registrationGate !== null);
  const rebindTask = watch(rebindExistingBrowserAlerts(() => true));
  readmitSameIdentity("cred-readmitted");
  check(registrationGate, "registration gate");
  registrationGate.finish(registration);
  await outcome(releaseTask);
  const rebound = await outcome(rebindTask);
  assertNo(matching("POST", REBIND_PATH), "queued rebind must not post after readmission");
  equal(localStorage.getItem(DEVICE_KEY), null, "queued rebind must not adopt a device id");
  assertStale(rebound, "queued rebind");
}

async function rebindHeldAtSubscription(): Promise<void> {
  currentSubscription = subscription;
  localStorage.setItem(DEVICE_KEY, "dev-before");
  arm = "subscription";
  const task = watch(rebindExistingBrowserAlerts(() => true));
  await until("rebind waits on getSubscription", () => subscriptionGate !== null);
  readmitSameIdentity("cred-readmitted");
  check(subscriptionGate, "subscription gate");
  subscriptionGate.finish(subscription);
  const result = await outcome(task);
  assertNo(matching("POST", REBIND_PATH), "held rebind must not post after readmission");
  equal(localStorage.getItem(DEVICE_KEY), "dev-before", "held rebind must not replace the stored device id");
  assertStale(result, "held rebind");
}

async function queuedPresenceAcrossReadmission(): Promise<void> {
  localStorage.setItem(DEVICE_KEY, "dev-present");
  currentSubscription = subscription;
  arm = "permission";
  const enableTask = watch(enableBrowserAlerts(PUBLIC_KEY, () => true));
  await until("queue holder waits on permission", () => permissionGate !== null);
  const presenceTask = watch(reportBrowserPresence("room-9", () => true));
  readmitSameIdentity("cred-readmitted");
  check(permissionGate, "permission gate");
  permissionGate.finish("granted");
  await outcome(enableTask);
  const presence = await outcome(presenceTask);
  assertNo(matching("POST", PRESENCE_PATH), "queued presence must not post after readmission");
  check(presence.ok, "retired presence does not surface as a new failure");
  equal(localStorage.getItem(DEVICE_KEY), "dev-present", "queued presence must not adopt a device id");
}

async function staleCleanupAfterReadmission(): Promise<void> {
  retireOnEnroll = true;
  const task = watch(enableBrowserAlerts(PUBLIC_KEY, () => {
    if (!retireArmed) return true;
    retireArmed = false;
    pausePrivateReads();
    setSessionKey("cred-readmitted");
    allowPrivateReads();
    return false;
  }));
  const result = await outcome(task);
  equal(matching("POST", ENROLL_PATH).length, 1, "the originating enrollment completed before retirement");
  assertNo(unenrollCalls(), "stale enable cleanup must not DELETE under the readmitted session");
  equal(localStorage.getItem(DEVICE_KEY), null, "stale enable cleanup must not adopt the enrolled device id");
  check(unsubscribeCalls >= 1, "stale enable cleanup still drops the subscription this gesture created");
  assertStale(result, "stale enable cleanup");
}

async function freshEnable(): Promise<void> {
  const result = await outcome(watch(enableBrowserAlerts(PUBLIC_KEY, () => true)));
  assertFulfilled(result, "current enable");
  equal(permissionCalls, 1, "current enable requests permission once");
  equal(subscribeCalls, 1, "current enable subscribes once");
  const enroll = matching("POST", ENROLL_PATH);
  equal(enroll.length, 1, "current enable enrolls once");
  equal(localStorage.getItem(DEVICE_KEY), "dev-enrolled", "current enable stores the enrolled device id");
}

async function deniedPermissionDoesNotEnroll(): Promise<void> {
  permissionResult = "denied";
  const result = await outcome(watch(enableBrowserAlerts(PUBLIC_KEY, () => true)));
  assertFulfilled(result, "denied permission");
  equal(permissionCalls, 1, "denied permission is not requested again");
  equal(subscribeCalls, 0, "denied permission does not subscribe");
  assertNo(matching("POST", ENROLL_PATH), "denied permission does not enroll");
  equal(localStorage.getItem(DEVICE_KEY), null, "denied permission does not store a device id");
}

async function callerStaleBeforeSubscribe(): Promise<void> {
  const result = await outcome(watch(enableBrowserAlerts(PUBLIC_KEY, () => false)));
  equal(permissionCalls, 0, "a retired caller cannot prompt for fresh browser consent");
  assertStale(result, "caller-stale enable");
  equal(subscribeCalls, 0, "a caller-stale enable does not subscribe");
  assertNo(matching("POST", ENROLL_PATH), "a caller-stale enable does not enroll");
}

async function freshDisable(): Promise<void> {
  localStorage.setItem(DEVICE_KEY, "dev-1");
  currentSubscription = subscription;
  const result = await outcome(watch(disableBrowserAlerts("dev-1")));
  assertFulfilled(result, "current disable");
  const deleted = unenrollCalls();
  equal(deleted.length, 1, "current disable deletes the enrollment");
  equal(localStorage.getItem(DEVICE_KEY), null, "current disable clears the local device id");
  check(unsubscribeCalls >= 1, "current disable drops the local subscription");
}

async function freshRelease(): Promise<void> {
  localStorage.setItem(DEVICE_KEY, "dev-release");
  currentSubscription = subscription;
  const result = await outcome(watch(releaseBrowserAlerts()));
  check(result.ok, "current release fulfills");
  const deleted = unenrollCalls();
  equal(deleted.length, 1, "current release deletes the enrollment");
  equal(localStorage.getItem(DEVICE_KEY), null, "current release clears the local device id");
  check(unsubscribeCalls >= 1, "current release drops the local subscription");
  check(notificationCloses >= 1, "current release closes visible notifications");
}

async function freshRebind(): Promise<void> {
  currentSubscription = subscription;
  const result = await outcome(watch(rebindExistingBrowserAlerts(() => true)));
  assertFulfilled(result, "current rebind");
  const rebound = matching("POST", REBIND_PATH);
  equal(rebound.length, 1, "current rebind posts once");
  equal(localStorage.getItem(DEVICE_KEY), "dev-rebound", "current rebind stores the rebound device id");
}

async function rebindNoMatchClearsLocal(): Promise<void> {
  currentSubscription = subscription;
  localStorage.setItem(DEVICE_KEY, "dev-before");
  rebindDeviceId = null;
  const result = await outcome(watch(rebindExistingBrowserAlerts(() => true)));
  assertFulfilled(result, "rebind no-match");
  equal(matching("POST", REBIND_PATH).length, 1, "a no-match still asks the current session");
  assertNo(unenrollCalls(), "a no-match does not send a subscription DELETE");
  equal(localStorage.getItem(DEVICE_KEY), null, "a no-match clears the local device id");
  check(unsubscribeCalls >= 1, "a no-match drops the local subscription");
  check(notificationCloses >= 1, "a no-match closes visible notifications");
}

async function freshPresence(): Promise<void> {
  localStorage.setItem(DEVICE_KEY, "dev-present");
  const result = await outcome(watch(reportBrowserPresence("room-9", () => true)));
  check(result.ok, "current presence fulfills");
  const presence = matching("POST", PRESENCE_PATH);
  equal(presence.length, 1, "current presence posts once");
}

async function presenceSkipsWhenCallerIsStale(): Promise<void> {
  localStorage.setItem(DEVICE_KEY, "dev-present");
  const result = await outcome(watch(reportBrowserPresence("room-9", () => false)));
  check(result.ok, "caller-stale presence fulfills");
  assertNo(matching("POST", PRESENCE_PATH), "caller-stale presence does not post");
}

async function sameSessionStaleUnenroll(): Promise<void> {
  const result = await outcome(watch(enableBrowserAlerts(PUBLIC_KEY, () => matching("POST", ENROLL_PATH).length === 0)));
  assertStale(result, "same-session stale enable");
  equal(matching("POST", ENROLL_PATH).length, 1, "same-session stale enable enrolled before the caller went stale");
  const deleted = unenrollCalls();
  equal(deleted.length, 1, "same-session stale enable takes the enrollment back");
  equal(localStorage.getItem(DEVICE_KEY), null, "same-session stale enable does not keep the device id");
  check(unsubscribeCalls >= 1, "same-session stale enable drops the subscription it created");
}

async function signedOutReleaseWithoutReadmission(): Promise<void> {
  pausePrivateReads();
  localStorage.setItem(DEVICE_KEY, "dev-signed-out");
  currentSubscription = subscription;
  const result = await outcome(watch(releaseBrowserAlerts()));
  check(result.ok, "signed-out release fulfills while reads stay denied");
  assertNo(unenrollCalls(), "signed-out release does not send a DELETE");
  equal(localStorage.getItem(DEVICE_KEY), null, "signed-out release clears the local device id");
  check(unsubscribeCalls >= 1, "signed-out release drops the local subscription");
  check(notificationCloses >= 1, "signed-out release closes visible notifications");
}

async function main(): Promise<void> {
  check(pushSupported(), "the harness exposes a secure push-capable browser");
  await scenario("disable held across readmission", disableHeldAcrossReadmission);
  await scenario("release held across readmission", releaseHeldAcrossReadmission);
  await scenario("signed-out release readmitted", releaseSignedOutThenReadmitted);
  await scenario("queued enable across readmission", queuedEnableAcrossReadmission);
  await scenario("enable held at permission", enableHeldAtPermission);
  await scenario("enable while denied", enableWhileDenied);
  await scenario("queued rebind across readmission", queuedRebindAcrossReadmission);
  await scenario("rebind held at subscription", rebindHeldAtSubscription);
  await scenario("queued presence across readmission", queuedPresenceAcrossReadmission);
  await scenario("stale cleanup after readmission", staleCleanupAfterReadmission);
  await scenario("current enable", freshEnable);
  await scenario("denied permission", deniedPermissionDoesNotEnroll);
  await scenario("caller stale before subscribe", callerStaleBeforeSubscribe);
  await scenario("current disable", freshDisable);
  await scenario("current release", freshRelease);
  await scenario("current rebind", freshRebind);
  await scenario("rebind no-match", rebindNoMatchClearsLocal);
  await scenario("current presence", freshPresence);
  await scenario("caller-stale presence", presenceSkipsWhenCallerIsStale);
  await scenario("same-session stale unenroll", sameSessionStaleUnenroll);
  await scenario("signed-out release without readmission", signedOutReleaseWithoutReadmission);
  if (failures.length > 0) throw new Error(failures.join("\n"));
  console.log("push opt-in lifetime ok");
}

main().catch((error: unknown) => {
  allowPrivateReads();
  console.error(error);
  const runtime = globalThis as typeof globalThis & { process?: { exitCode?: number } };
  if (runtime.process) runtime.process.exitCode = 1;
});
