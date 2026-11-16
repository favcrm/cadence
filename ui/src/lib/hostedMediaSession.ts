import { verifiedHostedSessionId } from "./privateSession";

const CAPABILITY = "cadence:media-capability";
const SESSION = "cadence:media-session";
const VERSION = 1;
const WORKER_TIMEOUT_MS = 6_000;

/** Answer only the controlling worker, with this tab's currently admitted witness. */
export function listenForHostedMediaSession(): () => void {
  if (typeof navigator === "undefined" || !("serviceWorker" in navigator)) return () => {};
  const service = navigator.serviceWorker;
  const message = (event: MessageEvent) => {
    if (!service.controller || event.source !== service.controller || event.data?.type !== SESSION || event.data?.version !== VERSION) return;
    const port = event.ports[0];
    if (!port) return;
    try {
      port.postMessage({ type: SESSION, version: VERSION, sessionId: verifiedHostedSessionId() });
    } finally {
      port.close();
    }
  };
  service.addEventListener("message", message);
  return () => service.removeEventListener("message", message);
}

/** A native video may request bytes only after its controlling worker proves v1 support. */
export function requireHostedMediaWorker(signal: AbortSignal): Promise<void> {
  if (typeof navigator === "undefined" || !("serviceWorker" in navigator) || typeof MessageChannel === "undefined") {
    return Promise.reject(new Error("Hosted media protection is unavailable"));
  }
  if (signal.aborted) return Promise.reject(new DOMException("The private session changed", "AbortError"));
  const service = navigator.serviceWorker;
  return new Promise((resolve, reject) => {
    let finished = false;
    let checked: ServiceWorker | null = null;
    const ports = new Set<MessagePort>();
    const finish = (error?: Error) => {
      if (finished) return;
      finished = true;
      clearTimeout(timer);
      signal.removeEventListener("abort", aborted);
      service.removeEventListener("controllerchange", check);
      for (const port of ports) port.close();
      ports.clear();
      if (error) reject(error);
      else resolve();
    };
    const aborted = () => finish(new DOMException("The private session changed", "AbortError"));
    const check = () => {
      const controller = service.controller;
      if (finished || !controller || controller === checked) return;
      checked = controller;
      const channel = new MessageChannel();
      ports.add(channel.port1);
      channel.port1.onmessage = (event: MessageEvent) => {
        ports.delete(channel.port1);
        channel.port1.close();
        if (event.data?.type === CAPABILITY && event.data?.version === VERSION && service.controller === controller) {
          finish();
        } else if (service.controller !== controller) {
          check();
        }
      };
      channel.port1.onmessageerror = () => {
        ports.delete(channel.port1);
        channel.port1.close();
      };
      try {
        controller.postMessage({ type: CAPABILITY, version: VERSION }, [channel.port2]);
      } catch {
        channel.port2.close();
        ports.delete(channel.port1);
        channel.port1.close();
      }
    };
    const timer = setTimeout(() => finish(new Error("Hosted media protection is unavailable")), WORKER_TIMEOUT_MS);
    signal.addEventListener("abort", aborted, { once: true });
    service.addEventListener("controllerchange", check);
    check();
    try {
      void service.register("/notifications-sw.js", { scope: "/" }).then((registration) => {
        check();
        // A previous controller may not implement v1. An update can replace it;
        // the deadline still fails closed if that update cannot be obtained.
        if (!finished) void registration.update().catch(() => {});
      }, (error: unknown) => finish(error instanceof Error ? error : new Error("Hosted media protection is unavailable")));
    } catch (error: unknown) {
      finish(error instanceof Error ? error : new Error("Hosted media protection is unavailable"));
    }
  });
}
