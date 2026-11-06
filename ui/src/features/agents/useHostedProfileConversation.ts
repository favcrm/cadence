import { useEffect, useRef, useState } from "react";
import { api } from "../../lib/api";
import { onPrivateReadsPaused, onPrivateSessionReset, withPrivateOperation } from "../../lib/privateSession";

type State =
  | { profile: string; kind: "loading" | "error" | "uncertain" | "paused" }
  | { profile: string; kind: "ready"; conversation: string; name: string; options: Binding[] };
type Binding = { id: string; revision: number; created: string | null };

const CONVERSATION = /^tc_[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const PENDING_PREFIX = "cadence:profile-conversation:uncertain:";
const SELECTED_PREFIX = "cadence:profile-conversation:selected:";

/** The ids seen before a create let a retry distinguish its result from an old conversation. */
function pending(profileId: string): string[] | null {
  try {
    const value = sessionStorage.getItem(PENDING_PREFIX + profileId);
    if (value === null) return null;
    const parsed: unknown = JSON.parse(value);
    if (Array.isArray(parsed) && parsed.every((id) => typeof id === "string" && CONVERSATION.test(id))) return parsed;
  } catch { /* Fail closed if the marker cannot be read. */ }
  return ["unavailable"];
}
function markPending(profileId: string, previous: Binding[]): boolean {
  try {
    sessionStorage.setItem(PENDING_PREFIX + profileId, JSON.stringify(previous.map(({ id }) => id)));
    return true;
  } catch { return false; }
}
function clearPending(profileId: string): void {
  try { sessionStorage.removeItem(PENDING_PREFIX + profileId); } catch { /* Reads still fail closed. */ }
}
onPrivateSessionReset(() => {
  try {
    for (let index = sessionStorage.length - 1; index >= 0; index--) {
      const key = sessionStorage.key(index);
      if (key?.startsWith(PENDING_PREFIX) || key?.startsWith(SELECTED_PREFIX)) sessionStorage.removeItem(key);
    }
  } catch { /* The marker contains no transcript or credential. */ }
});

/** A server-selected binding must name both the requested profile and its revision. */
function binding(value: unknown, profileId: string): Binding | null {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return null;
  const row = value as Record<string, unknown>;
  return row.profile_id === profileId && typeof row.id === "string" && CONVERSATION.test(row.id) &&
    typeof row.profile_revision === "number" && Number.isSafeInteger(row.profile_revision) && row.profile_revision > 0
    ? { id: row.id, revision: row.profile_revision, created: typeof row.created === "string" && Number.isFinite(Date.parse(row.created)) ? row.created : null } : null;
}

/** Resolve only this member's profile conversation, never Home as a fallback. */
export function useHostedProfileConversation(profileId: string, block: string | null) {
  const [state, setState] = useState<State>({ profile: profileId, kind: "loading" });
  const [attempt, setAttempt] = useState(0);
  const newRequested = useRef(false);
  const requested = useRef<string | null>(null);

  useEffect(() => {
    let active = true;
    const abort = new AbortController();
    setState({ profile: profileId, kind: "loading" });
    const unpause = onPrivateReadsPaused(() => {
      abort.abort();
      if (active) setState({ profile: profileId, kind: "paused" });
    });
    void withPrivateOperation(async (operation) => {
      const response = await api.profileConversations(operation);
      if (!active || abort.signal.aborted) return;
      if (!response || typeof response !== "object" || Array.isArray(response) ||
          !("conversations" in response) || !Array.isArray(response.conversations))
        throw new Error("Invalid conversation list");
      const matches: Binding[] = [];
      for (const row of response.conversations as unknown[]) {
        if (row && typeof row === "object" && !Array.isArray(row) && "profile_id" in row && row.profile_id === profileId) {
          const selected = binding(row, profileId);
          if (!selected) throw new Error("Invalid profile conversation binding");
          matches.push(selected);
        }
      }
      const show = async (selected: Binding, options: Binding[]) => {
        const result: unknown = await api.masterState(selected.id, operation);
        if (!result || typeof result !== "object" || !("actor_identity" in result)) throw new Error("Missing profile identity");
        const actor = result.actor_identity;
        if (!actor || typeof actor !== "object" || !("scope" in actor) || actor.scope !== "agent_profile" ||
            !("conversation_id" in actor) || actor.conversation_id !== selected.id ||
            !("identity" in actor) || !actor.identity || typeof actor.identity !== "object" ||
            !("id" in actor.identity) || actor.identity.id !== profileId ||
            !("revision" in actor.identity) || actor.identity.revision !== selected.revision ||
            !("name" in actor.identity) || typeof actor.identity.name !== "string" || !actor.identity.name.trim())
          throw new Error("Invalid profile identity");
        if (active && !abort.signal.aborted) {
          try { sessionStorage.setItem(SELECTED_PREFIX + profileId, selected.id); } catch { /* The active selection remains in memory. */ }
          requested.current = selected.id;
          setState({ profile: profileId, kind: "ready", conversation: selected.id, name: actor.identity.name, options });
        }
      };
      const previous = pending(profileId);
      let saved: string | null = null;
      try { saved = sessionStorage.getItem(SELECTED_PREFIX + profileId); } catch { /* No saved selection. */ }
      const wanted = requested.current ?? saved;
      const selected = previous === null
        ? wanted ? matches.find(({ id }) => id === wanted) : matches[0]
        : previous.includes("unavailable") ? undefined : matches.find(({ id }) => !previous.includes(id));
      if (!selected && previous === null && wanted && !newRequested.current)
        throw new Error("Selected conversation is unavailable");
      if (selected && !newRequested.current) {
        await show(selected, matches);
        if (active && !abort.signal.aborted) clearPending(profileId);
        return;
      }
      if (previous !== null) {
        if (active) setState({ profile: profileId, kind: "uncertain" });
        return;
      }
      if (block !== null) {
        if (active) setState({ profile: profileId, kind: "paused" });
        return;
      }
      newRequested.current = false;
      // Record the old set before sending: a dropped reply cannot prove the create did not land.
      if (!markPending(profileId, matches)) {
        if (active) setState({ profile: profileId, kind: "error" });
        return;
      }
      try {
        const answer = await api.createProfileConversation(profileId, operation);
        const created = answer && typeof answer === "object" && !Array.isArray(answer) && "conversation" in answer
          ? binding(answer.conversation, profileId) : null;
        if (!created || matches.some(({ id }) => id === created.id)) {
          if (active) setState({ profile: profileId, kind: "uncertain" });
          return;
        }
        await show(created, [created, ...matches]);
        if (active && !abort.signal.aborted) clearPending(profileId);
      } catch {
        if (!active || abort.signal.aborted) return;
        setState({ profile: profileId, kind: "uncertain" });
      }
    }, abort.signal).catch(() => {
      if (active && !abort.signal.aborted) setState({ profile: profileId, kind: "error" });
    });
    return () => { active = false; abort.abort(); unpause(); };
  }, [profileId, block, attempt]);

  const startNew = () => { newRequested.current = true; setAttempt((value) => value + 1); };
  return {
    state: state.profile === profileId ? state : { profile: profileId, kind: "loading" as const },
    retry: () => setAttempt((value) => value + 1),
    select: (id: string) => { requested.current = id; setAttempt((value) => value + 1); },
    startNew,
    createAnother: () => { clearPending(profileId); startNew(); },
  };
}
