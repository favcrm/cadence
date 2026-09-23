import { useEffect, useSyncExternalStore } from "react";
import type { Resource, ResourceState } from "./cache";

/** Subscribe a component to one resource store. */
export function useResource<T>(resource: Resource<T>): ResourceState<T> {
  return useSyncExternalStore(resource.subscribe, resource.get);
}

/**
 * Subscribe and revalidate on mount (stale-while-revalidate): the screen
 * paints whatever the store holds now — loading, or the last payload — and
 * the store refetches behind it unless its data is still fresh.
 */
export function useQuery<T>(resource: Resource<T>): ResourceState<T> {
  useEffect(() => {
    void resource.revalidate();
  }, [resource]);
  return useResource(resource);
}
