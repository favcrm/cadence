// CAD-1423: push and click handling only. No fetch handler, no cache, no
// offline behaviour. The server sends a generic payload by default; a preview
// appears only when the user opted in and the server redacted it. The click
// target is a same-origin thread path, re-checked here before use.

const THREAD_PREFIX = "/threads?h=";
const MAX_TITLE = 80;
const MAX_BODY = 140;

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
