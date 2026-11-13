import { useEffect, useState } from "react";
import { resources } from "../../lib/resources";
import { useMaybeResource } from "../../lib/useResource";
import { projectArchiveBlock, UNVERIFIED_WRITE_BLOCK } from "../projects/drawerArchiveGate";

/** `null` is unscoped; `undefined` is a project namespace without a provable key. */
export function wikiPathProject(path: string | null): string | null | undefined {
  if (path === null || (path !== "projects" && !path.startsWith("projects/"))) return null;
  const key = path.split("/")[1];
  return key && /^[a-z0-9][a-z0-9-]{0,31}$/.test(key) ? key : undefined;
}

/** A lifecycle receipt is a UI affordance, never a substitute for the server fence. */
export function useWikiArchiveBlock(path: string | null, typed = false): string | null {
  const scope = wikiPathProject(path);
  const project = typeof scope === "string" ? scope : null;
  // A destination can be typed character by character. Do not query a
  // succession of incomplete project keys; keep it blocked until it settles.
  const [settled, setSettled] = useState(project);
  useEffect(() => {
    if (!typed) {
      setSettled(project);
      return;
    }
    const timer = window.setTimeout(() => setSettled(project), 150);
    return () => window.clearTimeout(timer);
  }, [project, typed]);
  const resource = project !== null && project === settled ? resources.projectLifecycle(project) : null;
  const state = useMaybeResource(resource);
  useEffect(() => {
    if (resource) void resource.revalidate();
  }, [resource]);
  if (scope === undefined) return UNVERIFIED_WRITE_BLOCK;
  return project === null ? null : projectArchiveBlock(project, state);
}
