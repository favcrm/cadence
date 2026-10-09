import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../../../lib/api";
import { parseAppChat, type AppChat } from "./contract";

export interface FileUploadProjection {
  declared: boolean;
  available: boolean;
}

export interface AppChatProjection {
  chat: AppChat;
  /** UX hint only. The upload and every later read are re-proved natively. */
  fileUpload: FileUploadProjection;
}

interface LoadedProjection {
  key: string;
  projection: AppChatProjection | null;
  loading: boolean;
}

/**
 * Loads an installation's app-chat descriptor (CAD-1110). The daemon serves
 * it from the bundle at the digest the install consented to; this loader trusts
 * none of it: it validates the bytes with `parseAppChat`, requires the served
 * `digest` to equal the installation's CURRENT digest and the served and
 * declared `app` to equal the installation's kind. File-upload availability
 * is a separate native projection, never part of or authority for app-chat v1.
 */
const cache = new Map<string, AppChatProjection | null>();
let nextRequest = 0;
const latestRequest = new Map<string, number>();
const inflight = new Map<string, { id: number; promise: Promise<AppChatProjection | null> }>();

export async function loadAppChatProjection(
  installId: string,
  digest: string,
  kind: string,
  refresh = false,
): Promise<AppChatProjection | null> {
  const key = `${installId}\u0000${digest}\u0000${kind}`;
  if (!refresh && cache.has(key)) return cache.get(key) ?? null;
  const pending = inflight.get(key);
  if (!refresh && pending) return pending.promise;
  const requestId = ++nextRequest;
  latestRequest.set(key, requestId);
  const run = (async () => {
    let found: AppChatProjection | null = null;
    try {
      const served = await api.appChatDescriptor(installId);
      if (served.digest === digest && served.app === kind) {
        const chat = parseAppChat(served.descriptor);
        if (chat.app === kind) {
          const capability = served.file_upload;
          const declared = capability?.declared === true;
          found = {
            chat,
            fileUpload: {
              declared,
              available: declared && capability?.available === true,
            },
          };
        }
      }
    } catch {
      found = null;
    }
    if (latestRequest.get(key) === requestId) cache.set(key, found);
    return found;
  })().finally(() => {
    if (inflight.get(key)?.id === requestId) inflight.delete(key);
  });
  inflight.set(key, { id: requestId, promise: run });
  return run;
}

/** Backwards-compatible descriptor-only loader. */
export async function loadAppChat(installId: string, digest: string, kind: string): Promise<AppChat | null> {
  return (await loadAppChatProjection(installId, digest, kind))?.chat ?? null;
}

/** The descriptor and current live file-upload projection for one install. */
export function useAppChatProjection(
  installId: string,
  digest: string | null,
  kind: string | null,
): {
  projection: AppChatProjection | null;
  loading: boolean;
  refresh: () => Promise<AppChatProjection | null>;
} {
  const key = digest === null || kind === null ? null : `${installId}\u0000${digest}\u0000${kind}`;
  const [loaded, setLoaded] = useState<LoadedProjection | null>(null);
  const ownerRef = useRef<{ key: string | null; epoch: number }>({ key, epoch: 0 });
  if (ownerRef.current.key !== key) {
    ownerRef.current = { key, epoch: ownerRef.current.epoch + 1 };
  }
  const operationRef = useRef(0);
  const owner = ownerRef.current;
  const refresh = useCallback(async () => {
    if (digest === null || kind === null || key === null || ownerRef.current !== owner) return null;
    const operation = ++operationRef.current;
    setLoaded((previous) => {
      if (ownerRef.current !== owner || operationRef.current !== operation) return previous;
      // A re-proof keeps the last proven projection live (loading stays
      // false) so Attach never flickers off after an upload; only the
      // first load, or a real negative in the refreshed result, closes it.
      const held = previous?.key === key && previous.projection !== null;
      return { key, projection: held ? previous.projection : null, loading: !held };
    });
    const projection = await loadAppChatProjection(installId, digest, kind, true);
    if (ownerRef.current === owner && operationRef.current === operation) {
      setLoaded({ key, projection, loading: false });
    }
    return projection;
  }, [installId, digest, kind, key, owner]);

  useEffect(() => {
    if (key === null) return;
    void refresh();
  }, [key, refresh]);

  const current = key !== null && loaded?.key === key ? loaded : null;
  return {
    projection: current?.projection ?? null,
    loading: key !== null && (current === null || current.loading),
    refresh,
  };
}

/** Test seam: forget every cached descriptor. */
export function resetAppChatCache(): void {
  cache.clear();
  latestRequest.clear();
  inflight.clear();
}
