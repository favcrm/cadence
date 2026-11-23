import { hook, ToolTask, type Extension } from "@earendil-works/pi-durable";

export const DEFAULT_LOOP_GUARD_LIMIT = 4;

interface ToolLoopState {
  fingerprint: string;
  count: number;
}

/**
 * Deterministic key-sorted JSON serialization for comparing tool arguments
 * regardless of property insertion order or circular references.
 */
export function stableJson(value: unknown, seen = new WeakSet<object>()): string {
  if (value === null) return "null";
  if (value === undefined) return "undefined";
  const kind = typeof value;
  if (kind === "boolean" || kind === "string") return JSON.stringify(value);
  if (kind === "number") return Number.isFinite(value) ? JSON.stringify(value) : "null";
  if (kind === "bigint") return JSON.stringify(value.toString());
  if (kind !== "object") return "null";

  const obj = value as object;
  if (seen.has(obj)) return '"[Circular]"';
  seen.add(obj);
  try {
    const maybeToJson = (value as { toJSON?: unknown }).toJSON;
    if (typeof maybeToJson === "function") {
      return stableJson(maybeToJson.call(value), seen);
    }
    if (Array.isArray(value)) {
      const items = value.map((item) =>
        item === undefined || typeof item === "function" || typeof item === "symbol"
          ? "null"
          : stableJson(item, seen),
      );
      return `[${items.join(",")}]`;
    }
    const record = value as Record<string, unknown>;
    const keys = Object.keys(record).sort();
    const entries: string[] = [];
    for (const key of keys) {
      const prop = record[key];
      if (prop === undefined || typeof prop === "function" || typeof prop === "symbol") continue;
      entries.push(`${JSON.stringify(key)}:${stableJson(prop, seen)}`);
    }
    return `{${entries.join(",")}}`;
  } finally {
    seen.delete(obj);
  }
}

/**
 * Stops repeated identical `(toolName, args)` invocations within a turn after
 * `limit` consecutive attempts so a stalled model pivots or explains the blocker.
 */
export class LoopGuard {
  private readonly states = new Map<string, ToolLoopState>();
  private readonly limit: number;

  constructor(limit = DEFAULT_LOOP_GUARD_LIMIT) {
    this.limit = Math.max(1, Math.floor(limit));
  }

  record(
    conversationKey: string | number | undefined,
    toolName: string,
    args: unknown,
  ): string | undefined {
    const key = String(conversationKey ?? "default");
    const fingerprint = `${toolName}\0${stableJson(args)}`;
    const prev = this.states.get(key);
    if (prev && prev.fingerprint === fingerprint) {
      prev.count += 1;
      if (prev.count > this.limit) {
        return `Loop guard stopped repeated '${toolName}' call (${prev.count} consecutive identical attempts). Change parameters, switch to a different tool or skill, or explain the blocker to the user.`;
      }
      return undefined;
    }
    this.states.set(key, { fingerprint, count: 1 });
    return undefined;
  }

  reset(conversationKey?: string | number): void {
    if (conversationKey === undefined) {
      this.states.clear();
      return;
    }
    this.states.delete(String(conversationKey));
  }
}

/**
 * Pi `ToolTask` hook installed after `intentGuard` so policy/authority refusals
 * always win before loop detection runs.
 */
export function loopGuardExtension(guard: LoopGuard): Extension {
  return {
    name: "hosted-loop-guard",
    hooks: [
      hook(ToolTask, {
        beforeTool: (call, api) => {
          const block = guard.record(api.conversationId, call.name, call.arguments);
          return block ? { block } : undefined;
        },
      }),
    ],
  };
}
