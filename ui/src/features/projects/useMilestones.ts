import { useEffect, useRef } from "react";
import { resources } from "../../lib/resources";
import { useQuery } from "../../lib/useResource";

/** Shared roadmap cache follows tracker changes in Overview and Milestones. */
export function useMilestones(project: string, issuesAsOf: number | null) {
  const resource = resources.milestones(project);
  const state = useQuery(resource);
  const seen = useRef(issuesAsOf);
  useEffect(() => {
    if (seen.current === issuesAsOf) return;
    seen.current = issuesAsOf;
    void resource.invalidate();
  }, [issuesAsOf, resource]);
  return { state, retry: () => void resource.refresh() };
}
