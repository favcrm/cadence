import type { Meta } from "./types";

/**
 * Human rooms — DMs, groups, and the notification inbox — are served only
 * on the hosted public surface: `src/ui/serve.rs` dispatches
 * `/api/human-threads/*` inside its `public_host` branch and nowhere else.
 * A loopback or tailnet board has no such route, so its inbox must not
 * mount and must not poll a 404 (CAD-1704).
 */
export function humanThreadsAvailable(
  meta: Partial<Pick<Meta, "hosted" | "session">> | null | undefined,
): boolean {
  if (!meta) return false;
  return meta.hosted === true || meta.session?.origin === "public";
}
