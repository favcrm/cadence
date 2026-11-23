import { useEffect, useState } from "react";
import { api, type MasterPermissionRequest } from "../../../lib/api";
import { subscribeMasterChanged } from "../../../lib/masterChanged";
import { permissionIdFromText, subscribePermission } from "../../home/PermissionCard";
import type { ThreadEntry } from "../../home/thread";

/**
 * The newest still-pending master permission request named in this thread
 * (CAD-1700, CAD-2318), or null. With `enabled = false` (hosted boards), skips
 * calling `/api/master/permissions`.
 */
export function usePendingPermissionRequest(
  entries: ThreadEntry[],
  enabled = true,
): MasterPermissionRequest | null {
  const ids: string[] = [];
  for (let i = entries.length - 1; i >= 0; i--) {
    const e = entries[i];
    if (e.role !== "system" || e.payload?.source !== "permission") continue;
    const id = permissionIdFromText(e.text);
    if (id !== null && !ids.includes(id)) ids.push(id);
  }
  const key = enabled ? ids.join(",") : "";
  const [pending, setPending] = useState<MasterPermissionRequest | null>(null);
  useEffect(() => {
    if (key === "") {
      setPending(null);
      return;
    }
    const orderedIds = key.split(",");
    let alive = true;
    let revision = 0;
    const read = () => {
      const current = ++revision;
      api
        .permissionRules()
        .then((out) => {
          if (!alive || current !== revision) return;
          const byId = new Map(out.requests.map((r) => [r.id, r]));
          let match: MasterPermissionRequest | null = null;
          for (const id of orderedIds) {
            const req = byId.get(id);
            if (req?.status === "pending") {
              match = req;
              break;
            }
          }
          setPending(match);
        })
        .catch(() => {
          if (alive && current === revision) setPending(null);
        });
    };
    read();
    const offMaster = subscribeMasterChanged(read);
    const offLocal = subscribePermission(read);
    return () => {
      alive = false;
      offMaster();
      offLocal();
    };
  }, [key]);
  return pending;
}

/**
 * Whether a master permission request named in this thread still waits on
 * the operator (CAD-1700 "Needs your approval"). The request record is the
 * truth: the thread only names the id. Re-read on `master` frames (a filed
 * or decided request bumps the master stamp) and on a local decision; no
 * timer. Ids are read from the newest permission entries first.
 */
export function usePendingPermission(entries: ThreadEntry[], enabled = true): boolean {
  return usePendingPermissionRequest(entries, enabled) !== null;
}
