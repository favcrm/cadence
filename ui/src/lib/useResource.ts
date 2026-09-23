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

const noSubscribe = () => () => {};
const noState = () => null;

/** `useResource` for a store that may not exist (no drawer open). */
export function useMaybeResource<T>(resource: Resource<T> | null): ResourceState<T> | null {
  return useSyncExternalStore(resource ? resource.subscribe : noSubscribe, resource ? resource.get : noState);
}
