/**
 * The `master` stream frame (CAD-1700) names no data; it means the master's
 * state (turn, queue, assistant operations, permissions) moved. The
 * resource is invalidated by the stream router; this notifier lets
 * components that keep their own view of that state (the assistant
 * operations list, the conversation list) re-read without a timer.
 */
const masterListeners = new Set<() => void>();

export function noteMasterChanged(): void {
  for (const listener of masterListeners) listener();
}

export function subscribeMasterChanged(listener: () => void): () => void {
  masterListeners.add(listener);
  return () => {
    masterListeners.delete(listener);
  };
}
