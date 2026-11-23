import { useEffect, useMemo, useState } from "react";
import { privateReadsAllowed } from "../../lib/privateSession";
import { searchHumanPeople, type HumanPerson } from "../threads/humanThreadsClient";
import { slugifyHandle, type MentionCandidate } from "./mentions";

/** Load workspace teammates (excluding the signed-in user) for `@mention` pickers. */
export function useWorkspaceTeammates(enabled = true): HumanPerson[] {
  const [state, setState] = useState<{ actor: string | null; people: HumanPerson[] }>({
    actor: null,
    people: [],
  });

  useEffect(() => {
    if (!enabled || !privateReadsAllowed()) return;
    const controller = new AbortController();
    let live = true;
    void searchHumanPeople("", controller.signal)
      .then((page) => {
        if (!live) return;
        setState({ actor: page.actor ?? null, people: page.people ?? [] });
      })
      .catch(() => {
        if (!live) return;
        setState({ actor: null, people: [] });
      });
    return () => {
      live = false;
      controller.abort();
    };
  }, [enabled]);

  return useMemo(
    () => (state.actor ? state.people.filter((p) => p.id !== state.actor) : state.people),
    [state.actor, state.people],
  );
}

/** Convert workspace teammates into `MentionCandidate` entries. */
export function teammateMentionCandidates(
  people: readonly HumanPerson[],
  subtitle: string,
): MentionCandidate[] {
  return people.map((p) => ({
    id: p.id,
    handle: slugifyHandle(p.name) || p.subject,
    label: p.name,
    kind: "human",
    subtitle,
    active: true,
  }));
}
