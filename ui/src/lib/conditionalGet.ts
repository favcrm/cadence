/** Bounded validators, scoped to this tab's current operator credential.
 * 304 is useful only with a matching in-memory representation; no disk cache.
 */
export class ConditionalGet {
  private scope: string | null | undefined;
  private readonly entries = new Map<string, { etag: string; value: unknown }>();
  constructor(private readonly request: typeof fetch = fetch, private readonly identity: () => string | null = () => null) {}

  async get<T>(path: string, headers: Record<string, string>): Promise<{ response: Response; value?: T }> {
    const scope = this.identity();
    if (scope !== this.scope) {
      this.entries.clear();
      this.scope = scope;
    }
    const previous = this.entries.get(path);
    const response = await this.request(path, {
      headers: { ...headers, ...(previous ? { "If-None-Match": previous.etag } : {}) },
      cache: "no-store",
    });
    if (scope !== this.identity()) {
      throw new Error("Session changed during request; retry with the current session");
    }
    if (response.status === 304 && previous) {
      return { response, value: previous.value as T };
    }
    if (!response.ok) {
      this.entries.delete(path);
      return { response };
    }
    const value = await response.json() as T;
    if (scope !== this.identity()) {
      throw new Error("Session changed during response decoding; retry with the current session");
    }
    const etag = response.headers.get("ETag");
    if (etag && scope === this.identity()) {
      this.entries.delete(path);
      this.entries.set(path, { etag, value });
      if (this.entries.size > 64) this.entries.delete(this.entries.keys().next().value!);
    }
    return { response, value };
  }
}
