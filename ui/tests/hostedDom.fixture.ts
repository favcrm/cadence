export {};
/**
 * Shared happy-dom, React act, stubbed loader, and event-driven DOM/signal
 * helpers for CAD-2726 hosted UI tests.
 */
import type * as HappyDOM from "happy-dom";

declare function require<T>(name: string): T;

const DOM_GLOBALS = [
  "window",
  "document",
  "navigator",
  "Node",
  "Element",
  "HTMLElement",
  "HTMLInputElement",
  "HTMLTextAreaElement",
  "HTMLButtonElement",
  "HTMLFormElement",
  "SVGElement",
  "MutationObserver",
  "ResizeObserver",
  "Event",
  "MouseEvent",
  "KeyboardEvent",
  "location",
  "history",
  "localStorage",
  "sessionStorage",
  "getComputedStyle",
] as const;

const BOUND_GLOBALS = [
  "addEventListener",
  "removeEventListener",
  "requestAnimationFrame",
  "cancelAnimationFrame",
] as const;

let stubMarkdownEnabled = false;
let requireHooked = false;

export type ActFn = (cb: () => Promise<void>) => Promise<void>;

/** Install happy-dom globals and loader stubs before importing React UI modules. */
export function installHostedDom(
  url: string,
  options: { stubMarkdown?: boolean } = {},
): InstanceType<typeof HappyDOM.Window> {
  const { Window } = require<typeof HappyDOM>("happy-dom");
  const win = new Window({ url });
  const winRecord = win as unknown as Record<string, unknown>;
  for (const key of DOM_GLOBALS) {
    Object.defineProperty(globalThis, key, {
      value: key === "window" ? win : winRecord[key],
      configurable: true,
      writable: true,
    });
  }
  for (const key of BOUND_GLOBALS) {
    const fn = winRecord[key];
    if (typeof fn === "function") {
      Object.defineProperty(globalThis, key, {
        value: (fn as (...args: unknown[]) => unknown).bind(win),
        configurable: true,
        writable: true,
      });
    }
  }
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", {
    value: true,
    configurable: true,
  });

  if (options.stubMarkdown) stubMarkdownEnabled = true;
  if (!requireHooked) {
    requireHooked = true;
    const loader = require<{
      prototype: { require: (this: unknown, id: string) => unknown };
    }>("module");
    const originalRequire = loader.prototype.require;
    loader.prototype.require = function (this: unknown, id: string) {
      if (id.endsWith(".css")) return {};
      if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
      if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
      if (stubMarkdownEnabled && id === "remark-gfm") return { HugeiconsIcon: () => null };
      if (stubMarkdownEnabled && id === "react-markdown") {
        return { __esModule: true, default: () => null };
      }
      return originalRequire.call(this, id);
    };
  }

  return win;
}

export function assertCondition(value: unknown, label: string): asserts value {
  if (!value) throw new Error(`hosted UI check failed: ${label}`);
}

export function assertEqual(actual: unknown, expected: unknown, label: string): void {
  if (actual !== expected) {
    throw new Error(`${label}: expected ${String(expected)}, got ${String(actual)}`);
  }
}

/** Event-driven wait backed by MutationObserver + AbortSignal.timeout (no polling). */
export async function waitForDom(
  act: ActFn,
  container: HTMLElement,
  predicate: () => boolean,
  label: string,
  timeoutMs = 2_000,
): Promise<void> {
  if (predicate()) return;
  await act(async () => {
    if (predicate()) return;
    await new Promise<void>((resolve, reject) => {
      const timeoutSignal = AbortSignal.timeout(timeoutMs);
      let settled = false;
      const finish = (error?: Error) => {
        if (settled) return;
        settled = true;
        observer.disconnect();
        timeoutSignal.removeEventListener("abort", onTimeout);
        if (error) reject(error);
        else resolve();
      };
      const check = () => {
        if (predicate()) finish();
      };
      const onTimeout = () => finish(new Error(`Timed out waiting for DOM condition: ${label}`));
      const observer = new MutationObserver(check);
      observer.observe(container, {
        childList: true,
        subtree: true,
        characterData: true,
        attributes: true,
      });
      timeoutSignal.addEventListener("abort", onTimeout, { once: true });
      queueMicrotask(check);
    });
  });
}

export interface EventSignal<T = void> {
  emit: (value: T) => void;
  wait: (act: ActFn, label: string, timeoutMs?: number) => Promise<T>;
}

/** Event-driven one-shot queue for awaiting fetch/redirect callbacks inside React.act. */
export function createEventSignal<T = void>(): EventSignal<T> {
  const pending: T[] = [];
  let waiter: {
    resolve: (value: T) => void;
    reject: (err: Error) => void;
    cleanup: () => void;
  } | null = null;

  return {
    emit(value: T) {
      if (waiter) {
        const current = waiter;
        waiter = null;
        current.cleanup();
        current.resolve(value);
        return;
      }
      pending.push(value);
    },
    async wait(act: ActFn, label: string, timeoutMs = 2_000): Promise<T> {
      let captured: T | undefined;
      let hasCaptured = false;
      if (pending.length > 0) {
        captured = pending.shift() as T;
        hasCaptured = true;
      }
      await act(async () => {
        if (hasCaptured) {
          await Promise.resolve();
          return;
        }
        captured = await new Promise<T>((resolve, reject) => {
          const timeoutSignal = AbortSignal.timeout(timeoutMs);
          const cleanup = () => timeoutSignal.removeEventListener("abort", onTimeout);
          const onTimeout = () => {
            waiter = null;
            cleanup();
            reject(new Error(`Timed out waiting for event: ${label}`));
          };
          waiter = { resolve, reject, cleanup };
          timeoutSignal.addEventListener("abort", onTimeout, { once: true });
        });
      });
      return captured as T;
    },
  };
}
