// CAD-1015: loaded first by the owned RPC adapter. Plain JavaScript is
// intentional: the same implementation is exercised by node:test and Pi.
// No socket, credential access, timers, model calls or resource side effects.
import { VERSION } from "@earendil-works/pi-coding-agent";

export default function install(pi) {
  // These semantics were verified on this exact runtime: awaited lifecycle
  // boundaries and synchronous streaming custom-message queue admission.
  // An upgrade must be validated, not silently granted this capability.
  if (VERSION !== "1.0.0") return;
  let pending = null;
  let current = null;
  let accepting = false;
  const seen = new Map();

  const reset = () => {
    pending = null;
    current = null;
    accepting = false;
    seen.clear();
  };
  pi.on("session_start", reset);
  pi.on("session_shutdown", reset);
  pi.on("session_before_switch", reset);
  pi.on("agent_start", () => {
    // A retry within the same Cadence run keeps its existing binding.
    if (pending !== null) {
      current = pending;
      pending = null;
    }
    accepting = false;
  });
  pi.on("turn_start", () => { accepting = current !== null; });
  // Closing admission at turn_end, not only agent_settled, prevents input
  // during the final asynchronous listener/drain window. Load this guard
  // before other extensions whose turn_end handlers might yield to I/O.
  pi.on("turn_end", () => { accepting = false; });
  pi.on("agent_end", () => { accepting = false; });
  pi.on("agent_settled", reset);
  // Cancellation may leave an accepted message unconsumed in the core queue
  // or history. The adapter clears the queue before dispatching a successor;
  // this projection also excludes old guidance from every later model input.
  pi.on("context", event => ({
    messages: Array.isArray(event.messages) ? event.messages.filter(message =>
      message.role !== "custom" || message.customType !== "cadence-turn-guidance"
      || (current !== null && message.details?.turn === current)) : [],
  }));

  const receipt = (operation, request, turn, outcome, reason = null) => {
    pi.appendEntry("cadence-turn-input", {
      version: 1, operation, request, turn, outcome, reason,
    });
  };
  const parse = (args, operation) => {
    try {
      if (args.length > 4096) throw new Error("oversize");
      const value = JSON.parse(args);
      const allowed = operation === "steer" ? ["request", "turn", "text"] : ["request", "turn"];
      if (!value || typeof value !== "object" || Array.isArray(value)
          || Object.keys(value).some(key => !allowed.includes(key))
          || typeof value.request !== "string" || !value.request || value.request.length > 160
          || typeof value.turn !== "string" || !value.turn || value.turn.length > 160
          || (operation === "steer" && (typeof value.text !== "string"
              || !value.text.trim() || Array.from(value.text).length > 500))) {
        throw new Error("invalid");
      }
      return value;
    } catch {
      receipt(operation, null, null, "rejected", "invalid turn-input envelope");
      return null;
    }
  };

  pi.registerCommand("cadence-bind-turn", {
    description: "Internal owned-RPC turn binding; never starts model work",
    handler: async (args, ctx) => {
      const value = parse(args, "bind");
      if (value === null) return;
      if (ctx.mode !== "rpc" || !ctx.isIdle() || current !== null || pending !== null) {
        receipt("bind", value.request, value.turn, "rejected", "runtime is not available for a new binding");
        return;
      }
      pending = value.turn;
      receipt("bind", value.request, value.turn, "bound");
    },
  });

  pi.registerCommand("cadence-abandon-turn", {
    description: "Internal cleanup of an exact unused binding after prompt refusal",
    handler: async (args, ctx) => {
      const value = parse(args, "abandon");
      if (value === null) return;
      if (ctx.mode !== "rpc" || !ctx.isIdle() || accepting || current !== null) {
        receipt("abandon", value.request, value.turn, "rejected", "an active run cannot be abandoned");
        return;
      }
      if (pending !== value.turn) {
        receipt("abandon", value.request, value.turn, "skipped_inactive", "unused binding does not match");
        return;
      }
      pending = null;
      receipt("abandon", value.request, value.turn, "cleared");
    },
  });

  pi.registerCommand("cadence-steer-turn", {
    description: "Internal exact-turn guidance; idle/stale/closing runs are refused",
    handler: async (args, ctx) => {
      const value = parse(args, "steer");
      if (value === null) return;
      if (ctx.mode !== "rpc") {
        receipt("steer", value.request, value.turn, "rejected", "owned RPC mode required");
        return;
      }
      if (!accepting || ctx.isIdle() || ctx.signal?.aborted || current !== value.turn) {
        receipt("steer", value.request, value.turn, "skipped_inactive", "target turn is not accepting input");
        return;
      }
      const previous = seen.get(value.request);
      if (previous !== undefined) {
        receipt("steer", value.request, value.turn,
          previous === value.text ? "queued" : "rejected",
          previous === value.text ? "duplicate" : "message id changed content");
        return;
      }
      if (seen.size >= 64) {
        receipt("steer", value.request, value.turn, "rejected", "turn guidance limit reached");
        return;
      }
      // No await between the runtime predicate and enqueue. sendMessage's
      // streaming branch synchronously enters the current agent queue;
      // sendUserMessage would run async input handlers and can start a run.
      pi.sendMessage({
        customType: "cadence-turn-guidance",
        content: "Scoped guidance for the current objective (not approval or task completion):\n" + value.text,
        display: true,
        details: { request: value.request, turn: value.turn },
      }, { deliverAs: "steer" });
      seen.set(value.request, value.text);
      receipt("steer", value.request, value.turn, "queued");
    },
  });
}
