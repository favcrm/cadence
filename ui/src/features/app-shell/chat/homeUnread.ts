import { useEffect } from "react";
import { api } from "../../../lib/api";
import { subscribeMasterChanged } from "../../../lib/masterChanged";
import { privateReadsAllowed } from "../../../lib/privateSession";
import { setAssistantUnread } from "./assistantUnread";

/**
 * Home's "new reply" dot (CAD-1700). Home records the newest assistant
 * entry it has shown (`noteHomeSeen`); while the operator is elsewhere,
 * each `master` stream frame re-reads Home's tail once and lights the dot
 * when a newer assistant entry landed. Seen state is per browser
 * (localStorage), so opening Home in one tab clears it in every tab.
 */
const KEY = "cadence.assistant.seen.home";

function seenHome(): number {
  try {
    return Number(window.localStorage.getItem(KEY)) || 0;
  } catch {
    return 0;
  }
}

/** Home is on screen and has shown everything up to `seq`. */
export function noteHomeSeen(seq: number): void {
  try {
    if (seq <= seenHome()) return;
    window.localStorage.setItem(KEY, String(seq));
  } catch {
    // Storage unavailable: the dot clears for this tab only.
  }
  setAssistantUnread("home", false);
}

/** Newest assistant (non-operator) entry seq in a thread page, or 0. */
function newestReply(entries: unknown[]): number {
  let newest = 0;
  for (const raw of entries) {
    const e = raw as { seq?: unknown; role?: unknown };
    if (typeof e.seq === "number" && e.role !== "operator" && e.seq > newest) newest = e.seq;
  }
  return newest;
}

/** Mounted once by the board shell; inert on Home itself. */
export function useHomeUnread(onHome: boolean): void {
  useEffect(() => {
    if (onHome) {
      setAssistantUnread("home", false);
      return;
    }
    let alive = true;
    const check = () => {
      if (!privateReadsAllowed()) return;
      api
        .thread("master", { tail: true, limit: 20 })
        .then((page) => {
          if (alive) setAssistantUnread("home", newestReply(page.entries) > seenHome());
        })
        .catch(() => undefined);
    };
    const onStorage = (event: StorageEvent) => {
      if (event.key === KEY) check();
    };
    check();
    const off = subscribeMasterChanged(check);
    window.addEventListener("storage", onStorage);
    return () => {
      alive = false;
      off();
      window.removeEventListener("storage", onStorage);
    };
  }, [onHome]);
}
