import type { Models, ModelsSimpleStreamOptions } from "@earendil-works/pi-ai/models";
import type { HarnessOptions } from "@earendil-works/pi-durable";

type Telemetry = NonNullable<HarnessOptions["telemetry"]>;
type SpanOptions = Parameters<Telemetry["startSpan"]>[0];
type Span = Parameters<Parameters<Telemetry["startSpan"]>[1]>[0];
type ModelKind = "generation" | "compaction";
type Phase = { readonly kind: ModelKind; readonly task: number };
type Call = { readonly kind: "generation"; readonly task?: number } | Phase;

/**
 * Correlate a model call with the engine invocation that made it. Pi Durable
 * passes its task's phase span and abort signal to the model. The span is an
 * object this session authored from the scheduler's structured task fields;
 * it is never reconstructed from prompt text or a concurrent task scan.
 */
export class ModelTaskContext {
  private readonly phases = new WeakMap<object, Phase>();
  private readonly calls = new WeakMap<AbortSignal, Call | null>();

  private readonly startSpan = async <T>(
    options: SpanOptions,
    callback: (span: Span) => T | Promise<T>,
  ): Promise<T> => {
    const span: Span = {
      startSpan: this.startSpan,
      addLink() {},
      addEvent() {},
      setAttributes() {},
      setStatus() {},
    };
    const attributes = options.attributes;
    const task = attributes?.["task.id"];
    const name = attributes?.["task.name"];
    const phase = attributes?.phase;
    if (
      options.name === "pi.durable.phase" &&
      attributes?.kind === "run" &&
      typeof task === "number" &&
      Number.isSafeInteger(task) &&
      task > 0
    ) {
      if (name === "pi.generation" && phase === "request")
        this.phases.set(span, { kind: "generation", task });
      else if (name === "pi.compaction" && phase === "summarize")
        this.phases.set(span, { kind: "compaction", task });
    }
    return callback(span);
  };

  readonly telemetry: Telemetry = { startSpan: this.startSpan };

  /** Install after the operation and identity guards, preserving their checks. */
  install(models: Models): void {
    const stream = models.streamSimple.bind(models);
    const complete = models.completeSimple.bind(models);
    models.streamSimple = (model, context, options) => {
      this.bind(options, "generation");
      return stream(model, context, options);
    };
    models.completeSimple = (model, context, options) => {
      this.bind(options, "compaction");
      return complete(model, context, options);
    };
  }

  private bind(options: ModelsSimpleStreamOptions | undefined, kind: ModelKind): void {
    const signal = options?.signal;
    if (!signal) return;
    const phase = options.telemetry && this.phases.get(options.telemetry);
    // A session without this telemetry retains the existing guarded generation
    // path. A compaction may never borrow that fallback: absent proof refuses.
    const call: Call | null =
      phase?.kind === kind
        ? phase
        : kind === "generation" && phase === undefined
          ? { kind }
          : null;
    if (this.calls.has(signal)) {
      const previous = this.calls.get(signal);
      if (!previous || !call || previous.kind !== call.kind || previous.task !== call.task) {
        this.calls.set(signal, null);
        return;
      }
    }
    this.calls.set(signal, call);
  }

  read(signal: AbortSignal | undefined): Call | null {
    if (!signal || signal.aborted) return null;
    return this.calls.get(signal) ?? null;
  }
}
