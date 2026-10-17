import { Resource } from "../../../lib/cache";
import { PUSH_BYTES_MAX, pushBytes, type ActionArgs, type ReadObservation, type ScreenPush } from "./screenProtocol";

const READS = new Set(["records.list", "records.show", "segments.list", "segments.show", "content.list", "content.show"]);
function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object") return `{${Object.entries(value).sort(([a], [b]) => a.localeCompare(b)).map(([k, v]) => `${JSON.stringify(k)}:${canonical(v)}`).join(",")}}`;
  return JSON.stringify(value);
}
interface Entry { scope: string; domain: string; operation: string; args: ActionArgs; size: number; resource: Resource<unknown> }
const entries = new Map<string, Entry>();
const epochs = new Map<string, number>();
let session: string | null = null;
let epochCounter = 0;
let epochFloor = 0;
/** Host memory only. Neither a Resource snapshot nor its partition grants permission. */
export function admitDisplaySession(key: string): void {
  if (session !== key) { purgeDisplayCache(); session = key; }
}
export function purgeDisplayCache(): void { entries.clear(); epochs.clear(); session = null; epochFloor = ++epochCounter; }
export function displayEpoch(domain: string): number { return epochs.get(domain) ?? epochFloor; }
export function invalidateDisplay(domain: string): void {
  // All digest buckets, including rollback. In-flight pre-write reads cannot repopulate.
  epochs.set(domain, ++epochCounter);
  for (const [key, entry] of entries) if (entry.domain === domain) entries.delete(key);
  if (epochs.size > 32) { entries.clear(); epochs.clear(); epochFloor = ++epochCounter; }
}
export function observeRead(scope: string, sessionKey: string, domain: string, operation: string, args: ActionArgs, data: unknown, epoch: number): void {
  if (!session || session !== sessionKey || !READS.has(operation) || epoch !== displayEpoch(domain)) return;
  const row = { operation, args, data };
  const text = JSON.stringify(row);
  const size = new TextEncoder().encode(text).byteLength;
  if (size > 65536) return;
  const copy = JSON.parse(text) as ReadObservation;
  const key = canonical([scope, operation, args]);
  const resource = entries.get(key)?.resource ?? new Resource<unknown>(async () => { throw new Error("Display observations never fetch"); });
  resource.write(() => copy.data);
  entries.delete(key);
  entries.set(key, { scope, domain, operation, args: copy.args, size, resource });
  const scoped = [...entries].filter(([, e]) => e.scope === scope);
  for (const [old] of scoped.slice(0, Math.max(0, scoped.length - 16))) entries.delete(old);
  while (entries.size > 32 || [...entries.values()].reduce((n, e) => n + e.size, 0) > 4 * 1024 * 1024) entries.delete(entries.keys().next().value!);
}
export function withReadObservations(push: ScreenPush, scope: string, rights: readonly string[]): ScreenPush {
  const out: ScreenPush = { ...push, read_observations: [] };
  for (const entry of [...entries.values()].reverse()) {
    if (entry.scope !== scope || !rights.includes(entry.operation)) continue;
    const row = { operation: entry.operation, args: entry.args, data: entry.resource.get().data };
    const candidate = { ...out, read_observations: [...out.read_observations!, row] };
    if (pushBytes(candidate) <= PUSH_BYTES_MAX) out.read_observations!.push(row);
  }
  return out;
}
export const displayPlanKey = (args: ActionArgs): string => canonical({ csv_text: args.csv_text, preview_token: args.preview_token, request_id: args.request_id, decisions: args.decisions ?? [] });
