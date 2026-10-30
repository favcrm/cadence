import type { AppBinding, Installation, PublishDestination } from "./workspaceApps";
import type { ToastMsg } from "../../ui/Toast";

/** CAD-1290: connect Instagram through AgenticOS and choose the publishing account.
 *  CAD-1939: Facebook Pages too, with one publication binding per destination. */

/** The publishable toolkits (the daemon's own allowlist). */
export const PUBLISH_TOOLKITS = ["instagram", "facebook"] as const;
export type PublishToolkit = (typeof PUBLISH_TOOLKITS)[number];
export const isPublishToolkit = (value: unknown): value is PublishToolkit =>
  typeof value === "string" && (PUBLISH_TOOLKITS as readonly string[]).includes(value);

export type ConnectOutcome = "connected" | "failed" | "cancelled";
const OUTCOMES: readonly string[] = ["connected", "failed", "cancelled"];
/** The params AgenticOS adds to `return_to`; both are removed after they are read. */
const RETURN_PARAMS = ["aos_connect", "aos_publish", "toolkit"];

/** This board's host-drawn Settings page for the install (`?screen=native`), where the connected
 *  accounts and "Use for publishing" live. The host sends it as `return_to`; AgenticOS refuses any other origin.
 *  Keep the immutable installation pin so a return after reinstall cannot
 *  select the replacement merely because its package key is the same. */
export function connectReturnTo(origin: string, appKey: string, installId: string): string {
  return `${origin}/apps/${encodeURIComponent(appKey)}?screen=native&install=${encodeURIComponent(installId)}`;
}

/** The outcome AgenticOS reported on return, only for a publishable toolkit. Anything else is not ours. */
export function connectOutcome(search: string): ConnectOutcome | null {
  const query = new URLSearchParams(search);
  const value = query.get("aos_connect");
  return value !== null && OUTCOMES.includes(value) && isPublishToolkit(query.get("toolkit")) ? (value as ConnectOutcome) : null;
}

/** The query with the return params removed (the rest is kept). */
export function withoutConnectParams(search: string): string {
  const query = new URLSearchParams(search);
  for (const key of RETURN_PARAMS) query.delete(key);
  const rest = query.toString();
  return rest ? `?${rest}` : "";
}

/** After a connect, AgenticOS asks the owner to allow publishing; it reports `allowed` or `skipped`. */
export function publishAllowed(search: string): boolean {
  return new URLSearchParams(search).get("aos_publish") === "allowed";
}

export function connectToast(outcome: ConnectOutcome, allowed = false): ToastMsg {
  if (outcome === "connected" && allowed) return { kind: "ok", text: "Publishing allowed" };
  if (outcome === "connected") return { kind: "ok", text: "Account connected" };
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

/** CAD-1939: the live binding of one destination (and toolkit, when named) for the active context. */
export function destinationBinding(bindings: AppBinding[], slot: string, contextId: string | null, digest: string,
  destinationId: string, toolkit?: string): AppBinding | null {
  return bindings.find(value => value.slot === slot && value.state === "configured" && (value.context_id ?? null) === (contextId ?? null)
    && value.config.bundle_digest === digest && value.config.publish?.destination_id === destinationId
    && (toolkit === undefined || value.config.publish?.toolkit === toolkit)) ?? null;
}

/** The binding "Use for publishing" writes for a destination: its own, else the slot's binding that
 *  has no destination yet (which takes it, as before CAD-1939). Another destination's is never it. */
export function bindingToUse(bindings: AppBinding[], slot: string, contextId: string | null, digest: string,
  destinationId: string, toolkit?: string): AppBinding | null {
  return destinationBinding(bindings, slot, contextId, digest, destinationId, toolkit)
    ?? bindings.find(value => value.slot === slot && value.state === "configured" && (value.context_id ?? null) === (contextId ?? null)
      && value.config.bundle_digest === digest && !value.config.publish) ?? null;
}

/** CAD-1939: every live destination binding of the send slot for the active context. */
export function destinationBindings(bindings: AppBinding[], slot: string, contextId: string | null, digest: string): AppBinding[] {
  return bindings.filter(value => value.slot === slot && value.state === "configured" && (value.context_id ?? null) === (contextId ?? null)
    && value.config.bundle_digest === digest && typeof value.config.publish?.destination_id === "string");
}

export function publishingTo(binding: AppBinding | null, destinations: PublishDestination[]): PublishDestination | null {
  const id = binding?.config.publish?.destination_id;
  return id ? destinations.find(value => value.destination_id === id) ?? null : null;
}
