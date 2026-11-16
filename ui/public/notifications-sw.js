// CAD-1423: push and click handling, plus fail-closed hosted media ranges.
// No cache or offline behavior. Notification previews require user opt-in and
// server redaction; clicks remain restricted to same-origin threads.

const THREAD_PREFIX = "/threads?h=";
const MAX_TITLE = 80;
const MAX_BODY = 140;
const MEDIA_PATH = /^\/api\/chat\/attachments-bound\/v1\/chf-[0-9a-f]{32}$/;
const MEDIA_SESSION = "cadence:media-session";
const MEDIA_CAPABILITY = "cadence:media-capability";
const MEDIA_VERSION = 1;
const MEDIA_PROOF_TIMEOUT_MS = 2_000;

self.addEventListener("install", (event) => {
  event.waitUntil(self.skipWaiting());
});

self.addEventListener("activate", (event) => {
  event.waitUntil(self.clients.claim());
});

self.addEventListener("message", (event) => {
  if (event.data?.type !== MEDIA_CAPABILITY || event.data?.version !== MEDIA_VERSION) return;
  const port = event.ports?.[0];
  if (!port) return;
  try {
    port.postMessage({ type: MEDIA_CAPABILITY, version: MEDIA_VERSION });
  } finally {
    port.close();
  }
});

function unavailableMedia() {
  return new Response(null, { status: 503, headers: { "cache-control": "no-store" } });
}

function mediaSession(client) {
  return new Promise((resolve) => {
    const channel = new MessageChannel();
    let settled = false;
    const finish = (session) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      channel.port1.close();
      resolve(session);
    };
    const timer = setTimeout(() => finish(null), MEDIA_PROOF_TIMEOUT_MS);
    channel.port1.onmessage = (event) => {
      const data = event.data;
      const session = data?.type === MEDIA_SESSION && data?.version === MEDIA_VERSION &&
        typeof data.sessionId === "string" && /^[A-Za-z0-9_.:-]{1,256}$/.test(data.sessionId)
        ? data.sessionId
        : null;
      finish(session);
    };
    channel.port1.onmessageerror = () => finish(null);
    try {
      client.postMessage({ type: MEDIA_SESSION, version: MEDIA_VERSION }, [channel.port2]);
    } catch {
      channel.port2.close();
      finish(null);
    }
  });
}

function discardMedia(response) {
  if (!response?.body) return;
  try { void response.body.cancel().catch(() => {}); } catch { /* Nothing was exposed. */ }
}

async function liveMediaClient(id) {
  if (!id) return null;
  const client = await self.clients.get(id);
  if (!client || typeof client.url !== "string") return null;
  try {
    return new URL(client.url).origin === self.location.origin ? client : null;
  } catch {
    return null;
  }
}

async function pinnedMedia(event) {
  let response;
  try {
    const client = await liveMediaClient(event.clientId);
    if (!client) return unavailableMedia();
    const session = await mediaSession(client);
    if (!session) return unavailableMedia();
    const headers = new Headers(event.request.headers);
    headers.set("X-Cadence-Expected-Session", session);
    response = await fetch(new Request(event.request, {
      headers,
      mode: "same-origin",
      credentials: "same-origin",
      redirect: "error",
      cache: "no-store",
    }));
    if (![200, 206, 416].includes(response.status) ||
        response.headers.get("X-Cadence-Verified-Session") !== session) {
      discardMedia(response);
      return unavailableMedia();
    }
    const currentClient = await liveMediaClient(event.clientId);
    if (!currentClient || await mediaSession(currentClient) !== session) {
      discardMedia(response);
      return unavailableMedia();
    }
    return response;
  } catch {
    discardMedia(response);
    return unavailableMedia();
  }
}

self.addEventListener("fetch", (event) => {
  if (event.request.method !== "GET") return;
  const url = new URL(event.request.url);
  if (url.origin !== self.location.origin || !MEDIA_PATH.test(url.pathname)) return;
  event.respondWith(pinnedMedia(event));
});

function safeThreadUrl(value) {
  if (typeof value !== "string" || !value.startsWith(THREAD_PREFIX)) return "/threads";
  // Only the room id may follow; reject anything that could leave the thread route.
  const room = value.slice(THREAD_PREFIX.length);
  let decoded;
  try {
    decoded = decodeURIComponent(room);
  } catch (_) {
    return "/threads";
  }
  if (!/^[A-Za-z0-9_.:-]{1,128}$/.test(decoded)) return "/threads";
  return value;
}

self.addEventListener("push", (event) => {
  let data = {};
  try {
    data = event.data ? event.data.json() : {};
  } catch (_) {
    data = {};
  }
  const title = typeof data.title === "string" ? data.title.slice(0, MAX_TITLE) : "New message";
  // Body is absent unless the user opted in to message text; then it is bounded.
  const body = typeof data.body === "string" ? data.body.slice(0, MAX_BODY) : "Open Cadence to read it.";
  const tag = typeof data.tag === "string" ? data.tag.slice(0, 128) : undefined;
  event.waitUntil(
    self.registration.showNotification(title, {
      body,
      tag,
      // Coalesce a burst for one thread into the newest alert, without re-alerting.
      renotify: false,
      // Sound follows the user's own preference, sent by the server as `silent`.
      silent: data.silent !== false,
      data: { url: safeThreadUrl(data.url) },
    }),
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const target = safeThreadUrl(event.notification.data && event.notification.data.url);
  event.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((clients) => {
      for (const client of clients) {
        if (new URL(client.url).origin !== self.location.origin) continue;
        // Route the already-open board to the exact thread, then focus it. The
        // page validates the target again and applies normal room authorization.
        return client.focus().then((focused) => {
          (focused || client).postMessage({ type: "cadence-open-thread", url: target });
          return focused;
        });
      }
      return self.clients.openWindow(target);
    }),
  );
});
