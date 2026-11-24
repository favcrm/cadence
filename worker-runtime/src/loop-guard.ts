import { hook, ToolTask, type Extension, type ToolExecutionResult } from "@earendil-works/pi-durable";

export const DEFAULT_LOOP_GUARD_LIMIT = 4;
export const DEFAULT_ERROR_LOOP_LIMIT = 3;

interface ToolLoopState {
  fingerprint: string;
  count: number;
}

interface ErrorLoopState {
  toolName: string;
  signature: string;
  count: number;
  sample: string;
}

function normalizeErrorSignature(message: string): string {
  const firstLine =
    message
      .split("\n")
      .map((line) => line.trim())
      .find((line) => line !== "") ?? "";
  return firstLine
    .replace(/^\[error\]\s*/i, "")
    .replace(/\/\d+(?=\/|$|:|\s)/g, "/#")
    .replace(/\s+/g, " ")
    .trim()
    .toLowerCase()
    .slice(0, 160);
}

function extractToolError(
  result: ToolExecutionResult,
): { message: string; blocked: boolean } | null {
  const errorDiag = result.diagnostics?.find((d) => d.severity === "error");
  if (errorDiag) {
    return { message: errorDiag.message, blocked: errorDiag.code === "blocked" };
  }
  if (!result.isError) return null;
  const textPart = result.output?.find((part) => part.type === "text");
  const text = textPart && "text" in textPart ? String(textPart.text) : "";
  return { message: text.trim() || "tool_error", blocked: false };
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
 * Stops repeated identical `(toolName, args)` invocations AND repeated same-tool
 * consecutive errors (even with slightly varied parameters) within a turn so a
 * stalled model pivots or explains the blocker.
 */
export class LoopGuard {
  private readonly states = new Map<string, ToolLoopState>();
  private readonly errorStates = new Map<string, ErrorLoopState>();
  private readonly limit: number;
  private readonly errorLimit: number;

  constructor(limit = DEFAULT_LOOP_GUARD_LIMIT, errorLimit = DEFAULT_ERROR_LOOP_LIMIT) {
    this.limit = Math.max(1, Math.floor(limit));
    this.errorLimit = Math.max(1, Math.floor(errorLimit));
  }

  record(
    conversationKey: string | number | undefined,
    toolName: string,
    args: unknown,
  ): string | undefined {
    const key = String(conversationKey ?? "default");
    const prevErr = this.errorStates.get(key);
    if (prevErr) {
      if (prevErr.toolName !== toolName) {
        this.errorStates.delete(key);
      } else if (prevErr.count >= this.errorLimit) {
        return `Loop guard stopped repeated failing '${toolName}' calls (${prevErr.count} consecutive identical errors: ${prevErr.sample}). Fix the failing parameter or root cause, switch to a different tool or skill, or explain the blocker to the user.`;
      }
    }
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

  recordResult(
    conversationKey: string | number | undefined,
    toolName: string,
    result: ToolExecutionResult,
  ): { warn?: string } {
    const key = String(conversationKey ?? "default");
    const err = extractToolError(result);
    if (!err) {
      this.errorStates.delete(key);
      return {};
    }
    if (err.blocked) {
      return {};
    }
    const signature = `${toolName}\0${normalizeErrorSignature(err.message)}`;
    const prev = this.errorStates.get(key);
    if (prev && prev.signature === signature) {
      prev.count += 1;
      prev.sample = err.message.trim().slice(0, 160);
      if (prev.count >= this.errorLimit) {
        return {
          warn: `Repeated failure (${prev.count} consecutive identical errors) on '${toolName}'. Do not retry with minor rewording — fix the missing or invalid input, switch approach, or explain the issue to the user.`,
        };
      }
      return {};
    }
    this.errorStates.set(key, {
      toolName,
      signature,
      count: 1,
      sample: err.message.trim().slice(0, 160),
    });
    return {};
  }

  reset(conversationKey?: string | number): void {
    if (conversationKey === undefined) {
      this.states.clear();
      this.errorStates.clear();
      return;
    }
    const key = String(conversationKey);
    this.states.delete(key);
    this.errorStates.delete(key);
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
        afterTool: (call, result, api) => {
          const outcome = guard.recordResult(api.conversationId, call.name, result);
          if (!outcome.warn) return undefined;
          return {
            ...result,
            diagnostics: [
              ...(result.diagnostics ?? []),
              { severity: "warn", code: "loop_guard_warning", message: outcome.warn },
            ],
          };
        },
      }),
    ],
  };
}
