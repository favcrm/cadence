import { useSyncExternalStore } from "react";
import type { MasterState } from "../../../lib/types";
import { conversationList } from "../conversationClient";
import { knownInstalls, onKnownInstallsChanged } from "./chatVisibility";

function subscribe(listener: () => void): () => void {
  let offLists: (() => void)[] = [];
  const observe = () => {
    offLists.forEach((off) => off());
    offLists = knownInstalls().map((id) => conversationList(id).subscribe(listener));
    listener();
  };
  const offInstalls = onKnownInstallsChanged(observe);
  observe();
  return () => {
    offInstalls();
    offLists.forEach((off) => off());
  };
}

/** Resolve the actual queued predecessor, not whichever turn happens to run.
 * Late list reads can supply an app destination; an unknown owner has no link. */
export function useConversationDestination(
  conversation: string | null,
  master: MasterState | null | undefined,
  homeConversation: string | undefined,
): string | null {
  return useSyncExternalStore(subscribe, () => {
    if (conversation === null || conversation === homeConversation) return "/";
    const origin = master?.turn?.app_origin;
    const runningOwner = master?.turn?.conversation === conversation && origin?.conversation_id === conversation
      ? origin.install_id
      : undefined;
    const install = runningOwner ?? knownInstalls().find((id) =>
      conversationList(id).get().data?.conversations.some((row) => row.id === conversation),
    );
    if (install === undefined) return null;
    const params = new URLSearchParams({ conversation });
    if (runningOwner !== undefined && origin?.context_id) params.set("ctx", origin.context_id);
    return `/app-installations/${encodeURIComponent(install)}?${params.toString()}`;
  });
}
