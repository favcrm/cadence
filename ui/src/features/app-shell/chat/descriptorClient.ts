import { useEffect, useState } from "react";
import { api } from "../../../lib/api";
import { parseAppChat, type AppChat } from "./contract";

/**
 * Loads an installation's app-chat descriptor (CAD-1110). The daemon serves
 * it from the bundle at the digest the operator approved; this loader trusts
 * none of it: it validates the bytes with `parseAppChat`, requires the served
 * `digest` to equal the installation's CURRENT digest and the served and
 * declared `app` to equal the installation's kind, and caches by
 * `(installId, digest)`. Any 404, error, mismatch or violation is `null`:
 * plain shared chat, never a throw into the pane and never a partial apply.
 */
const cache = new Map<string, AppChat | null>();
const inflight = new Map<string, Promise<AppChat | null>>();

export async function loadAppChat(installId: string, digest: string, kind: string): Promise<AppChat | null> {
  const key = `${installId}\u0000${digest}\u0000${kind}`;
  if (cache.has(key)) return cache.get(key) ?? null;
  const pending = inflight.get(key);
  if (pending) return pending;
  const run = (async () => {
    let found: AppChat | null = null;
    try {
      const served = await api.appChatDescriptor(installId);
      if (served.digest === digest && served.app === kind) {
        const parsed = parseAppChat(served.descriptor);
        if (parsed.app === kind) found = parsed;
      }
    } catch {
      found = null;
    }
    cache.set(key, found);
    return found;
  })().finally(() => inflight.delete(key));
  inflight.set(key, run);
  return run;
}

/** The descriptor for the pane's installation, or `null` (plain chat) while
 *  it loads, when the installation is unknown or unapproved, or when the
 *  served descriptor is refused. A late answer for a previous installation
 *  or digest is discarded. */
export function useAppChat(installId: string, digest: string | null, kind: string | null): AppChat | null {
  const [loaded, setLoaded] = useState<{ key: string; chat: AppChat | null } | null>(null);
  const key = digest === null || kind === null ? null : `${installId}\u0000${digest}\u0000${kind}`;
  useEffect(() => {
    if (digest === null || kind === null || key === null) return;
    let live = true;
    void loadAppChat(installId, digest, kind).then((chat) => {
      if (live) setLoaded({ key, chat });
    });
    return () => {
      live = false;
    };
  }, [installId, digest, kind, key]);
  return key !== null && loaded?.key === key ? loaded.chat : null;
}

/** Test seam: forget every cached descriptor. */
export function resetAppChatCache(): void {
  cache.clear();
  inflight.clear();
}
