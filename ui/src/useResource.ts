import { useSyncExternalStore } from "react";
import type { Resource, ResourceState } from "./resource";

/** Subscribe a component to one resource store. */
export function useResource<T>(resource: Resource<T>): ResourceState<T> {
  return useSyncExternalStore(resource.subscribe, resource.get);
}
