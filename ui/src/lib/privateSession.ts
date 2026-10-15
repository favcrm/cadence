/** In-memory admission and generation for private observations, never a credential. */
let admitted = true;
let generation = 0;
const resets = new Set<() => void>();
const pauses = new Set<() => void>();

export const privateReadsAllowed = (): boolean => admitted;
export const privateSessionGeneration = (): number => generation;

const SESSION_REQUIRED = "Session verification required";

function sessionChanged(): DOMException {
  return new DOMException("The private session changed", "AbortError");
}

function abortTarget(target: AbortController, reason: unknown): void {
  if (target.signal.aborted) return;
  if (reason === undefined) target.abort();
  else target.abort(reason);
}

/**
 * Forwards `external` into `target` without AbortSignal.any (Chrome 111 /
 * Firefox 114 / Safari 16.4). A signal that is already aborted aborts
 * `target` immediately and attaches nothing. A target that is already
 * aborted attaches nothing. The returned function removes both listeners.
 */
export function bindPrivateAbort(target: AbortController, external: AbortSignal): () => void {
  if (external.aborted) {
    abortTarget(target, external.reason);
    return () => {};
  }
  if (target.signal.aborted) return () => {};
  let active = true;
  const onExternal = () => {
    abortTarget(target, external.reason);
    stop();
  };
  const onTarget = () => stop();
  const stop = () => {
    if (!active) return;
    active = false;
    external.removeEventListener("abort", onExternal);
    target.signal.removeEventListener("abort", onTarget);
  };
  external.addEventListener("abort", onExternal);
  target.signal.addEventListener("abort", onTarget);
  if (external.aborted) {
    abortTarget(target, external.reason);
    stop();
  } else if (target.signal.aborted) {
    stop();
  }
  return stop;
}

interface OperationState {
  controller: AbortController;
  generation: number;
  disposed: boolean;
  /** Request controllers kept until the operation ends so body reads still abort. */
  requests: Map<AbortController, () => void>;
}

const states = new WeakMap<PrivateOperation, OperationState>();
const live = new Set<OperationState>();

function abandon(state: OperationState): void {
  const entries = [...state.requests.entries()];
  state.requests.clear();
  for (const [request, stop] of entries) {
    abortTarget(request, state.controller.signal.reason);
    stop();
  }
}

function operationCurrent(state: OperationState): boolean {
  return !state.disposed && !state.controller.signal.aborted && admitted && state.generation === generation;
}

/** One private continuation. Pause and reset abort it for good; resume does not revive it. */
export interface PrivateOperation {
  /** Aborts on pause, reset, dispose, or the external signal passed to begin. */
  readonly signal: AbortSignal;
  /** Generation captured when the operation began. */
  readonly generation: number;
  /** Throws AbortError when this continuation was paused, reset, disposed, or not admitted. */
  assertCurrent(): void;
  /** Aborts the signal, cancels in-flight bodies, and drops listeners. Idempotent. */
  dispose(): void;
  /** Same as `privateFetch(input, init, operation)`. */
  fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response>;
}

function stateOf(operation: PrivateOperation): OperationState {
  const state = states.get(operation);
  if (!state) throw new Error(SESSION_REQUIRED);
  return state;
}

/** Starts a continuation. Refuses while reads are paused. `external` aborts the whole continuation. */
export function beginPrivateOperation(external?: AbortSignal | null): PrivateOperation {
  if (!admitted) throw new Error(SESSION_REQUIRED);
  const started = generation;
  const controller = new AbortController();
  const state: OperationState = { controller, generation: started, disposed: false, requests: new Map() };
  controller.signal.addEventListener("abort", () => {
    live.delete(state);
    abandon(state);
  }, { once: true });
  const stopExternal = external ? bindPrivateAbort(controller, external) : () => {};
  const operation: PrivateOperation = {
    signal: controller.signal,
    generation: started,
    assertCurrent() {
      if (!operationCurrent(state)) throw sessionChanged();
    },
    dispose() {
      if (state.disposed) return;
      state.disposed = true;
      stopExternal();
      live.delete(state);
      if (!controller.signal.aborted) controller.abort(sessionChanged());
      else abandon(state);
    },
    fetch(input, init) {
      return privateFetch(input, init, operation);
    },
  };
  states.set(operation, state);
  if (!controller.signal.aborted) live.add(state);
  return operation;
}

/** Runs `fn` on a fresh operation and disposes it after `fn` settles, including body reads inside `fn`. */
export async function withPrivateOperation<T>(
  fn: (operation: PrivateOperation) => Promise<T>,
  external?: AbortSignal | null,
): Promise<T> {
  const operation = beginPrivateOperation(external);
  try {
    operation.assertCurrent();
    const value = await fn(operation);
    operation.assertCurrent();
    return value;
  } finally {
    operation.dispose();
  }
}

/**
 * Uses a caller-owned operation without disposing it. When `operation` is
 * omitted, begins one and disposes it after `fn` (and its body reads) settle.
 */
export function usingPrivateOperation<T>(
  operation: PrivateOperation | undefined,
  fn: (operation: PrivateOperation) => Promise<T>,
  external?: AbortSignal | null,
): Promise<T> {
  if (!operation) return withPrivateOperation(fn, external);
  return Promise.resolve().then(async () => {
    if (external && external !== operation.signal) bindPrivateAbort(stateOf(operation).controller, external);
    operation.assertCurrent();
    const value = await fn(operation);
    operation.assertCurrent();
    return value;
  });
}

function stringField(record: object, key: string): string | undefined {
  if (!Object.prototype.hasOwnProperty.call(record, key)) return undefined;
  const value: unknown = Reflect.get(record, key);
  return typeof value === "string" ? value : undefined;
}

/** Session-required code carried as top-level `code` (local) or nested `error.code` (hosted). */
function privateSessionCode(body: unknown): string | undefined {
  if (body === null || typeof body !== "object" || Array.isArray(body)) return undefined;
  const direct = stringField(body, "code");
  if (direct !== undefined) return direct;
  const error: unknown = Reflect.get(body, "error");
  if (error === null || typeof error !== "object" || Array.isArray(error)) return undefined;
  return stringField(error, "code");
}

/**
 * Dispatches `cadence:session-required` for an authoritative 401 whose body
 * carries `operator_session_required` or `board_session_required`, and only
 * while `operation` is still the admitted continuation. A paused, reset, or
 * disposed operation cannot log out the session that replaced it.
 */
export function notifyPrivateSessionRequired(status: number, body: unknown, operation: PrivateOperation): void {
  if (status !== 401 || typeof window === "undefined") return;
  try {
    operation.assertCurrent();
  } catch {
    return;
  }
  const code = privateSessionCode(body);
  if (code === "operator_session_required" || code === "board_session_required") {
    window.dispatchEvent(new Event("cadence:session-required"));
  }
}

function requestInputSignal(input: RequestInfo | URL): AbortSignal | null {
  if (typeof Request !== "function" || !(input instanceof Request)) return null;
  return input.signal;
}

function releaseRequest(state: OperationState, request: AbortController, stop: () => void): void {
  if (!request.signal.aborted) request.abort(sessionChanged());
  stop();
  state.requests.delete(request);
}

function abandoned(state: OperationState, request: AbortController): boolean {
  return request.signal.aborted || !operationCurrent(state);
}

/** Fetch bound to `operation` for the header response and the caller's later body read.
 * The effective caller signal abandons the whole operation. An explicit init.signal
 * overrides a Request's signal, matching fetch. The raw Response is returned unread. */
export async function privateFetch(
  input: RequestInfo | URL,
  init: RequestInit | undefined,
  operation: PrivateOperation,
): Promise<Response> {
  if (!admitted) throw new Error(SESSION_REQUIRED);
  const state = stateOf(operation);
  operation.assertCurrent();
  const caller = init?.signal === undefined ? requestInputSignal(input) : init.signal;
  if (caller && caller !== state.controller.signal) bindPrivateAbort(state.controller, caller);
  operation.assertCurrent();
  const request = new AbortController();
  let stopped = false;
  let onRequestAbort = () => {};
  const stop = () => {
    if (stopped) return;
    stopped = true;
    request.signal.removeEventListener("abort", onRequestAbort);
    state.requests.delete(request);
  };
  onRequestAbort = () => stop();
  if (state.disposed || state.controller.signal.aborted || request.signal.aborted) {
    abortTarget(request, state.controller.signal.aborted ? state.controller.signal.reason : request.signal.reason);
    stop();
    throw sessionChanged();
  }
  state.requests.set(request, stop);
  request.signal.addEventListener("abort", onRequestAbort, { once: true });
  if (request.signal.aborted || state.controller.signal.aborted) {
    stop();
    throw sessionChanged();
  }
  try {
    const response = await fetch(input, { ...init, signal: request.signal });
    if (abandoned(state, request)) {
      releaseRequest(state, request, stop);
      throw sessionChanged();
    }
    if (response.status === 401 && typeof window !== "undefined") {
      const decoded: unknown = await response.clone().json().catch(() => null);
      if (abandoned(state, request)) {
        releaseRequest(state, request, stop);
        throw sessionChanged();
      }
      notifyPrivateSessionRequired(response.status, decoded, operation);
    }
    if (abandoned(state, request)) {
      releaseRequest(state, request, stop);
      throw sessionChanged();
    }
    return response;
  } catch (error) {
    releaseRequest(state, request, stop);
    throw error;
  }
}

export function pausePrivateReads(): void {
  admitted = false;
  for (const state of [...live]) {
    if (!state.controller.signal.aborted) state.controller.abort(sessionChanged());
  }
  for (const pause of [...pauses]) pause();
}

export function allowPrivateReads(): void {
  admitted = true;
}

/** Confirmed identity loss/change, not an unanswered verification. */
export function resetPrivateSession(): void {
  pausePrivateReads();
  generation += 1;
  for (const reset of [...resets]) reset();
}

export function onPrivateSessionReset(reset: () => void): () => void {
  resets.add(reset);
  return () => { resets.delete(reset); };
}

export function onPrivateReadsPaused(pause: () => void): () => void {
  pauses.add(pause);
  return () => { pauses.delete(pause); };
}
