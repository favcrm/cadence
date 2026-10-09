import type { AppBinding, Installation, PublishDestination } from "./workspaceApps";
import type { ToastMsg } from "../../ui/Toast";

/** CAD-1290: connect Instagram through AgenticOS and choose the publishing account. */

export type ConnectOutcome = "connected" | "failed" | "cancelled";
const OUTCOMES: readonly string[] = ["connected", "failed", "cancelled"];
/** The params AgenticOS adds to `return_to`; both are removed after they are read. */
const RETURN_PARAMS = ["aos_connect", "toolkit"];

/** This board's host-drawn Settings page for the install (`?screen=native`), where the connected
 *  accounts and "Use for publishing" live. The host sends it as `return_to`; AgenticOS refuses any other origin. */
export function connectReturnTo(origin: string, installId: string): string {
  return `${origin}/app-installations/${encodeURIComponent(installId)}?screen=native`;
}

/** The outcome AgenticOS reported on return, only for Instagram. Anything else is not ours. */
export function connectOutcome(search: string): ConnectOutcome | null {
  const query = new URLSearchParams(search);
  const value = query.get("aos_connect");
  return value !== null && OUTCOMES.includes(value) && query.get("toolkit") === "instagram" ? (value as ConnectOutcome) : null;
}

/** The query with the return params removed (the rest is kept). */
export function withoutConnectParams(search: string): string {
  const query = new URLSearchParams(search);
  for (const key of RETURN_PARAMS) query.delete(key);
  const rest = query.toString();
  return rest ? `?${rest}` : "";
}

export function connectToast(outcome: ConnectOutcome): ToastMsg {
  if (outcome === "connected") return { kind: "ok", text: "Instagram connected" };
  // The cancel signal is unverified upstream: cancelled reads as not connected, like failed.
  return { kind: "err", text: "Connection failed — try again" };
}

/** "@harbour" for a handle; a name with spaces is shown as it is. */
export function accountLabel(label: string): string {
  const text = label.trim();
  return /^[A-Za-z0-9._]+$/.test(text) ? `@${text}` : text;
}

/** The one send slot the app declares, which "Use for publishing" binds. */
export function sendSlot(installation: Installation): string | null {
  const sends = Object.entries(installation.capabilities ?? {}).filter(([, need]) => need.effect === "send");
  return sends.length === 1 ? sends[0][0] : null;
}

/** The live publication binding for the active context, and the account it publishes to. */
export function publicationBinding(bindings: AppBinding[], slot: string, contextId: string | null, digest: string): AppBinding | null {
  return bindings.find(value => value.slot === slot && value.state === "configured" && (value.context_id ?? null) === (contextId ?? null)
    && value.config.bundle_digest === digest) ?? null;
}

export function publishingTo(binding: AppBinding | null, destinations: PublishDestination[]): PublishDestination | null {
  const id = binding?.config.publish?.destination_id;
  return id ? destinations.find(value => value.destination_id === id) ?? null : null;
}
