import { useEffect, useState } from "react";
import { api } from "../../../lib/api";
import { subscribeMasterChanged } from "../../../lib/masterChanged";
import { permissionIdFromText, subscribePermission } from "../../home/PermissionCard";
import type { ThreadEntry } from "../../home/thread";

/**
 * Whether a master permission request named in this thread still waits on
 * the operator (CAD-1700 "Needs your approval"). The request record is the
 * truth: the thread only names the id. Re-read on `master` frames (a filed
 * or decided request bumps the master stamp) and on a local decision; no
 * timer. Ids are read from the newest permission entries first.
 */
export function usePendingPermission(entries: ThreadEntry[]): boolean {
  const ids = entries
    .filter((e) => e.role === "system" && e.payload?.source === "permission")
    .map((e) => permissionIdFromText(e.text))
    .filter((id): id is string => id !== null);
  const key = ids.join(",");
  const [pending, setPending] = useState(false);
  useEffect(() => {
    if (key === "") {
      setPending(false);
      return;
    }
    const wanted = new Set(key.split(","));
    let alive = true;
    const read = () => {
      api
        .permissionRules()
        .then((out) => {
          if (alive) setPending(out.requests.some((r) => wanted.has(r.id) && r.status === "pending"));
        })
        .catch(() => undefined);
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
