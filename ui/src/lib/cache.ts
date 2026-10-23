/** HOST resource stores retain native private-session admission and retirement. */
import { Resource as CoreResource, QueryCache as CoreQueryCache, type ResourceOptions } from "./cacheCore";
import { onPrivateReadsPaused, onPrivateSessionReset, privateReadsAllowed } from "./privateSession";

export type { ResourceStatus, ResourceState, ResourceOptions } from "./cacheCore";

export class Resource<T> extends CoreResource<T> {
  constructor(fetcher: () => Promise<T>, opts: ResourceOptions<T> = {}) {
    super(fetcher, opts, privateReadsAllowed);
  }
}

export class QueryCache extends CoreQueryCache {
  constructor(opts: { maxIdle?: number } = {}) {
    super(opts);
    onPrivateReadsPaused(() => {
      for (const entry of this.entries.values()) entry.pause();
    });
    onPrivateSessionReset(() => {
      for (const entry of this.entries.values()) entry.reset();
    });
  }

  protected override createResource<T>(fetcher: () => Promise<T>, opts?: ResourceOptions<T>): Resource<T> {
    return new Resource(fetcher, opts);
  }
}

/** Resource names the server's `/api/stream` events carry in `resources`. */
export const RESOURCE_NAMES = [
  "issues",
  "projects",
  "agents",
  "overview",
  "issue",
  "workflows",
  "apps",
  "app",
  "app_runs",
  "app_outputs",
  "outbox",
  "threads",
  "master",
] as const;
export type ResourceName = (typeof RESOURCE_NAMES)[number];

/**
 * Which resources one stream event invalidates. The server names them in
 * the event data (`{"resources":["agents","overview"]}`); a server that
 * predates the field sends `{}`, and then everything is refetched — the
 * old behaviour, never a missed update.
 */
export function invalidatedBy(data: string | null | undefined): ResourceName[] {
  try {
    const parsed = JSON.parse(data ?? "") as { resources?: unknown };
    if (Array.isArray(parsed.resources)) {
      const known = new Set<string>(RESOURCE_NAMES);
      return parsed.resources.filter(
        (r): r is ResourceName => typeof r === "string" && known.has(r),
      );
    }
  } catch {
    // Unparseable data — fall through to the conservative answer.
  }
  return [...RESOURCE_NAMES];
}
