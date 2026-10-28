import { useEffect, useState } from "react";
import { api } from "../../lib/api";
import type { ProtectedEntry } from "../../lib/types";

/**
 * CAD-1085: the deployment's trusted credential origin, read from the
 * board's metadata route (which resolves it from the root profile through
 * the daemon — never from a request field or env value).
 *
 * `origin: null` means trusted entry is unsupported on this deployment;
 * callers must say so and must never fall back to a board-relative form.
 * A failed read is treated the same way by the credential surfaces.
 */
export type ProtectedEntryState =
  | { status: "loading"; entry: null }
  | { status: "failed"; entry: null }
  | { status: "ready"; entry: ProtectedEntry };

export function useProtectedEntry(): ProtectedEntryState {
  const [state, setState] = useState<ProtectedEntryState>({
    status: "loading",
    entry: null,
  });
  useEffect(() => {
    let live = true;
    const controller = new AbortController();
    api
      .protectedEntry(controller.signal)
      .then((entry) => {
        if (live) setState({ status: "ready", entry });
      })
      .catch(() => {
        if (live) setState({ status: "failed", entry: null });
      });
    return () => {
      live = false;
      controller.abort();
    };
  }, []);
  return state;
}

/** True only on the exact pinned origin with a configured entry surface. */
export function onTrustedOrigin(state: ProtectedEntryState): boolean {
  return (
    state.status === "ready" &&
    state.entry.origin !== null &&
    state.entry.configured &&
    window.location.origin === state.entry.origin
  );
}
