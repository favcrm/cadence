import type { AppBinding, Installation, PublishDestination } from "./workspaceApps";
import type { ToastMsg } from "../../ui/Toast";
import { DRAWER_SURFACE, type ScreenDrawerSize } from "./screen/screenProtocol";

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

/** Where the operator comes back to. `native` is the host-drawn Settings page of this board
 *  (the native workspace's own callers). `screen` is the app's own screen — the one a
 *  drawer surface lives on — and `drawer` additionally names the surface to open again, so a
 *  connect started in the app's drawer returns to that drawer instead of another page. */
export type ConnectReturnTarget =
  | { mode: "native" }
  | { mode: "screen" }
  | { mode: "drawer"; surface: string; size?: ScreenDrawerSize };

/** This board's install page: its host-drawn Settings page for the install (`?screen=native`),
 *  the app screen itself, or that screen with the drawer surface the owner connected from.
 *  AgenticOS refuses any other origin; it keeps the path and query we send. Keep the immutable
 *  installation pin so a return after reinstall cannot select the replacement merely because
 *  its package key is the same. The starting context travels as the board's own `ctx` claim:
 *  the shell still makes the workspace's active context the authority, but it notices when the
 *  claim no longer matches instead of silently opening the drawer in another scope. */
export function connectReturnTo(
  origin: string,
  appKey: string,
  installId: string,
  target: ConnectReturnTarget = { mode: "native" },
  contextId?: string,
): string {
  const query = new URLSearchParams();
  if (target.mode === "native") query.set("screen", "native");
  else if (target.mode === "drawer" && DRAWER_SURFACE.test(target.surface)) {
    query.set("drawer", target.surface);
    if (target.size) query.set("drawer_size", target.size);
  }
  query.set("install", installId);
  if (contextId) query.set("ctx", contextId);
  return `${origin}/apps/${encodeURIComponent(appKey)}?${query.toString()}`;
}

/** The query parameters that carry the return drawer back. The board owns them: the app frame
 *  never writes them, and the shell re-opens the surface only after its own frame handshakes. */
export const RETURN_DRAWER_PARAMS = ["drawer", "drawer_size"] as const;

/** The drawer surface a connect return asked the board to re-open, or null for a page. A
 *  surface that is not one a drawer request could name is ignored; the app screen still opens. */
export function readReturnDrawer(search: string): { surface: string; size?: ScreenDrawerSize } | null {
  const query = new URLSearchParams(search);
  const surface = query.get("drawer");
  if (surface === null || !DRAWER_SURFACE.test(surface)) return null;
  const size = query.get("drawer_size");
  return size === "narrow" || size === "wide" ? { surface, size } : { surface };
}

/** The query with the return-drawer hint removed (the rest is kept). */
export function withoutReturnDrawer(search: string): string {
  const query = new URLSearchParams(search);
  for (const key of RETURN_DRAWER_PARAMS) query.delete(key);
  const rest = query.toString();
  return rest ? `?${rest}` : "";
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

/** What the return says. The URL is a report from another origin, not evidence: `connected` is
 *  neutral about the account (the scoped account read shows what really exists), and neither a
 *  connection nor any publishing grant is claimed from a query parameter. */
export function connectToast(outcome: ConnectOutcome): ToastMsg {
  if (outcome === "connected") return { kind: "ok", text: "Connection finished — check your accounts" };
  // The cancel signal is unverified upstream: cancelled reads as not connected, like failed.
  return { kind: "err", text: "Connection failed — try again" };
}

/** A source-save completion may only refresh the primary whose install and context it was made
 *  in: after the operator switches scope, the settled write must not touch the new screen. */
export function sameScope(
  initiating: { installId: string; contextId: string },
  current: { installId: string; contextId: string } | null,
): boolean {
  return current !== null && current.installId === initiating.installId && current.contextId === initiating.contextId;
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
