export interface ScreenBoot {
  tag: string;
  bridge_nonce: string;
  generation: string;
  parent_origin: string;
}

export interface ScreenPush {
  v: 1 | 2;
  op: "screen";
  tag: string;
  install_id: string;
  digest: string;
  context_id: string;
  [key: string]: unknown;
}

export interface ScreenReceiverOptions {
  boot?: ScreenBoot;
  onPush(push: ScreenPush): void;
  onError?(reason: string): void;
  accepts?: readonly ("screen.v1" | "screen.v2")[];
  timeoutMs?: number;
  document?: { getElementById(id: string): { textContent: string | null } | null };
  parent?: { postMessage(message: unknown, origin: string, transfer?: unknown[]): void };
  window?: { addEventListener?(type: string, fn: () => void, options?: object): void };
  MessageChannel?: typeof MessageChannel;
}

export interface SlotState {
  state?: "pending" | "done" | "refused";
  receipt?: unknown;
  data?: unknown;
  refusal?: { code: string; text: string };
}

export interface ScreenReceiver {
  boot: ScreenBoot | null;
  accepted(): boolean;
  call<T = unknown>(verb: string, args?: unknown): Promise<T>;
  slot(verb: string, args?: unknown, anchor?: unknown): Promise<SlotState>;
  tool<T = unknown>(alias: string, input?: unknown, requestId?: string): Promise<T>;
  asset(ref: string): Promise<string>;
  state(data: string): boolean;
  teardown(): void;
}

export declare class ScreenRefusalError extends Error {
  name: "ScreenRefusalError";
  code: string;
  text: string;
  status: number;
  constructor(refusal: { code?: string; text?: string }, statuses?: Record<string, number>);
}

export declare class ScreenClosedError extends Error {
  name: "ScreenClosedError";
  code: "gone";
  status: 410;
  constructor(message?: string);
}

export declare class ScreenTimeoutError extends Error {
  name: "ScreenTimeoutError";
  code: "timeout";
  status: 504;
  id: string;
  constructor(id: string, timeoutMs: number);
}

export declare const REFUSAL_STATUS: Readonly<Record<string, number>>;
export declare const MAX_STATE_BYTES: number;
export declare const MAX_ASSET_BYTES: number;
export declare const DEFAULT_TIMEOUT_MS: number;

export declare function readRefusal(value: unknown): { code: string; text: string } | null;
export declare function validateBoot(value: unknown): ScreenBoot;
export declare function readBoot(documentRef?: unknown): ScreenBoot;
export declare function createScreenReceiver(options: ScreenReceiverOptions): ScreenReceiver;
