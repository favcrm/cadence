import { useEffect, useMemo, useState } from "react";
import { privateReadsAllowed } from "../../lib/privateSession";
import { searchHumanPeople, type HumanPerson } from "../threads/humanThreadsClient";
import { slugifyHandle, type MentionCandidate } from "./mentions";

/** Exclude the signed-in caller by either derived `actor` id or auth `subject`. */
export function filterSelfTeammates(
  people: readonly HumanPerson[],
  actorId: string | null,
  viewerSub?: string | null,
): HumanPerson[] {
  return people.filter(
    (p) =>
      (actorId === null || p.id !== actorId) &&
      (viewerSub == null || viewerSub === "" || p.subject !== viewerSub),
  );
}

/** Load both all workspace members (for assignee pickers) and filtered teammates (for @mentions). */
export function useWorkspacePeopleDirectory(
  enabled = true,
  viewerSub?: string | null,
): { people: HumanPerson[]; teammates: HumanPerson[] } {
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

  const teammates = useMemo(
    () => filterSelfTeammates(state.people, state.actor, viewerSub),
    [state.actor, state.people, viewerSub],
  );
  return { people: state.people, teammates };
}

/** Load workspace teammates (excluding the signed-in user) for `@mention` pickers. */
export function useWorkspaceTeammates(enabled = true, viewerSub?: string | null): HumanPerson[] {
  return useWorkspacePeopleDirectory(enabled, viewerSub).teammates;
}

/** Convert workspace teammates into `MentionCandidate` entries, disambiguating colliding handles. */
export function teammateMentionCandidates(
  people: readonly HumanPerson[],
  subtitle: string,
): MentionCandidate[] {
  const resolved = people.map((p) => {
    const explicitHandle = (p as HumanPerson & { handle?: string }).handle?.trim();
    const handle = explicitHandle || slugifyHandle(p.name) || p.subject;
    return { person: p, handle, key: handle.toLowerCase() };
  });
  const counts = new Map<string, number>();
  for (const row of resolved) {
    counts.set(row.key, (counts.get(row.key) ?? 0) + 1);
  }
  return resolved.map(({ person, handle, key }) => {
    const duplicate = (counts.get(key) ?? 0) > 1;
    const suffix = duplicate ? person.subject.slice(-6) : "";
    const resolvedSubtitle =
      duplicate && suffix
        ? subtitle
          ? `${subtitle} · ${suffix}`
          : suffix
        : subtitle || undefined;
    return {
      id: person.id,
      handle,
      label: person.name,
      kind: "human",
      ...(resolvedSubtitle ? { subtitle: resolvedSubtitle } : {}),
      active: true,
    };
  });
}
