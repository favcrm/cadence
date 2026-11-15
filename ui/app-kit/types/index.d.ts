export interface ScreenBoot {
  tag: string;
  bridge_nonce: string;
  generation: number;
  parent_origin: string;
}

export interface ScreenInstallation {
  install_id: string;
  name: string;
  title: string;
  version: string;
  digest: string;
  summary: string;
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

export type ScreenPresentationMode = "outlet" | "drawer";
export type ScreenSurfaceRole = "primary" | "drawer";
export type ScreenLocale = "en" | "zh-TW";
export type ScreenAudioMode = "record" | "upload";
export type ScreenAudioSource = "recording" | "upload";
export type ScreenAudioLanguage = "" | "en" | "zh";
export type ScreenAudioSessionPhase =
  | "idle"
  | "starting"
  | "recording"
  | "paused"
  | "stopping"
  | "transcribing"
  | "ready"
  | "error";
export type ScreenCapability =
  | "screen.v1"
  | "screen.v2"
  | "screen-actions.v1"
  | "screen-media.v1"
  | "screen-presentation.v1"
  | "screen-surfaces.v1"
  | "screen-records.v1"
  | "screen-locale.v1"
  | "screen-audio.v1"
  | "screen-audio.v2";

export interface ScreenAudioCapture {
  readonly transcript: string;
  readonly duration_seconds: number;
  readonly held_at: string;
  readonly source: ScreenAudioSource;
}

export interface ScreenAudioCaptureOptions {
  mode?: ScreenAudioMode;
  timeoutMs?: number;
}

export interface ScreenAudioSessionSnapshot {
  readonly session_id: string;
  readonly mode: ScreenAudioMode;
  readonly phase: ScreenAudioSessionPhase;
  readonly held_at: string;
  readonly elapsed_seconds: number;
  readonly level: number;
  readonly peak: number;
  readonly segments_total: number;
  readonly segments_done: number;
  readonly segments_in_flight: number;
  readonly segments_failed: number;
  readonly no_speech_segments: number;
  readonly transcript: string;
  readonly error: { readonly code: string; readonly text: string } | null;
}

export type ScreenAudioSessionCommand =
  | { action: "start"; language?: ScreenAudioLanguage; timeoutMs?: number }
  | { action: "pause" | "resume" | "stop" | "retry" | "cancel"; timeoutMs?: number }
  | {
      action: "upload";
      bytes: ArrayBuffer;
      mimeType: string;
      fileName?: string;
      lastModified?: number;
      language?: ScreenAudioLanguage;
      timeoutMs?: number;
    };

export type SlotAnchor = "footer" | { x: number; y: number; w: number; h: number };

export interface ScreenReceiverOptions {
  boot?: ScreenBoot;
  /** Optional expected screen tag; boot fails closed if `boot.tag` differs. */
  tag?: string;
  onPush(push: ScreenPush): void;
  onError?(reason: string): void;
  onSlotState?(event: SlotState): void;
  onAudioSessionEvent?(state: ScreenAudioSessionSnapshot): void;
  onPresentation?(mode: ScreenPresentationMode): void;
  onSurface?(role: ScreenSurfaceRole, surface: string | undefined, modal: boolean): void;
  accepts?: readonly ScreenCapability[];
  /** Largest JSON args/input bag to send. Defaults to 16 KiB. */
  maxArgsBytes?: number;
  timeoutMs?: number;
  document?: { getElementById(id: string): { textContent: string | null } | null };
  parent?: { postMessage(message: unknown, origin: string, transfer?: unknown[]): void };
  window?: { addEventListener?(type: string, fn: () => void, options?: object): void };
  MessageChannel?: typeof MessageChannel;
}

export interface GenerationScope {
  draft_id: string;
  operation: "caption" | "image";
  revision: number;
}

export interface SlotState {
  state: "pending" | "done" | "refused";
  receipt?: unknown;
  data?: unknown;
  refusal?: { code: string; text: string };
}

export interface ScreenLeaveHandle {
  publish(unsaved: boolean): boolean;
  release(): void;
}

export interface ScreenDrawerOpenOptions {
  size?: "narrow" | "wide";
  context?: unknown;
}

export interface ScreenReceiver {
  boot: ScreenBoot | null;
  accepted(): boolean;
  call<T = unknown>(verb: string, args?: unknown): Promise<T>;
  slot(verb: string, args?: unknown, anchor?: SlotAnchor): Promise<SlotState>;
  tool<T = unknown>(alias: string, input?: unknown, requestId?: string, generationScope?: GenerationScope): Promise<T>;
  audioCapture(options?: ScreenAudioCaptureOptions): Promise<ScreenAudioCapture>;
  audioSession(command: ScreenAudioSessionCommand): Promise<ScreenAudioSessionSnapshot>;
  asset(ref: string, options?: { timeoutMs?: number }): Promise<string>;
  assetAdmitted(ref: string): boolean;
  state(data: string): boolean;
  ownLeaveState(options?: { isLive?: () => boolean }): ScreenLeaveHandle | null;
  presentation(): ScreenPresentationMode;
  surfaceRole(): ScreenSurfaceRole;
  surface(): string | undefined;
  openDrawer(surface: string, options?: ScreenDrawerOpenOptions): Promise<unknown>;
  closeDrawer(): Promise<unknown>;
  drawerContext<T = unknown>(): Promise<T | null>;
  teardown(): void;
}

export interface ScreenScope {
  installId: string;
  digest: string;
  contextId?: string;
}

export interface ScreenRoute<Section extends string = string, View extends string = "list" | "new"> {
  section: Section;
  view: View;
  record_id: string | null;
  segment_id: string | null;
}

export interface ScreenRoutePatch<Section extends string = string, View extends string = "list" | "new"> {
  section?: Section;
  view?: View;
  record_id?: string | null;
  segment_id?: string | null;
}

export interface ScreenReadObservation {
  operation: string;
  args: Record<string, unknown>;
  data: unknown;
}

export interface ScreenSurfaceSnapshot {
  readonly role: ScreenSurfaceRole;
  readonly name: string | undefined;
  readonly modal: boolean;
}

export interface ScreenScopeSnapshot<
  Section extends string = string,
  View extends string = "list" | "new",
> {
  readonly scope: Required<ScreenScope>;
  readonly digest: string;
  readonly actions: readonly string[];
  readonly audioCapture: boolean;
  readonly presentationLocale: ScreenLocale;
  readonly route: ScreenRoute<Section, View> | null;
  readonly observations: readonly ScreenReadObservation[];
  readonly contexts: readonly { readonly id: string; readonly label: string }[];
  readonly installation: Readonly<ScreenInstallation>;
  readonly raw: ScreenPush;
}

export interface ScreenBridgeState<
  Section extends string = string,
  View extends string = "list" | "new",
  Err extends Error = ScreenBridgeError,
> {
  readonly status: "loading" | "ready" | "error";
  readonly snapshot: ScreenScopeSnapshot<Section, View> | null;
  readonly error: Err | null;
  readonly pushes: number;
  readonly surface: ScreenSurfaceSnapshot;
}

export interface ScreenBridgeRuntimeOverrides {
  boot?: ScreenBoot;
  document?: ScreenReceiverOptions["document"];
  parent?: ScreenReceiverOptions["parent"];
  window?: ScreenReceiverOptions["window"];
  MessageChannel?: typeof MessageChannel;
  onSlotState?: (event: SlotState) => void;
}

export interface ScreenBridgeCallOptions {
  quiet?: boolean;
  keepAliveOnForbidden?: boolean;
}

export interface ScreenBridgeOptions<
  Section extends string = string,
  View extends string = "list" | "new",
  Err extends Error = ScreenBridgeError,
> {
  tag: string;
  appName?: string;
  records?: boolean;
  actions?: boolean;
  media?: boolean;
  surfaces?: boolean;
  presentation?: boolean;
  locale?: boolean;
  audio?: boolean | "v1" | "v2";
  calls?: Record<string, readonly string[]>;
  slots?: Record<string, readonly string[]>;
  argBytes?: number | Record<string, number>;
  singleFlight?: readonly string[];
  writeVerbs?: readonly string[];
  route?: {
    sections: readonly Section[];
    defaultSection?: Section;
    views?: readonly View[];
  };
  observations?: readonly string[];
  pushKeys?: readonly string[];
  maxInFlight?: number;
  timeoutMs?: number;
  createError?: (message: string, status: number, options?: { code?: string }) => Err;
}

export interface ScreenBridge<
  Section extends string = string,
  View extends string = "list" | "new",
  Err extends Error = ScreenBridgeError,
> {
  readonly tag: string;
  readonly operations: readonly string[];
  subscribe(listener: () => void): () => void;
  getState(): ScreenBridgeState<Section, View, Err>;
  getSnapshot(): ScreenScopeSnapshot<Section, View> | null;
  can(verb: string, actions?: readonly string[]): boolean;
  canAudioCapture(snapshot?: ScreenScopeSnapshot<Section, View> | null): boolean;
  assertScope(scope: ScreenScope): void;
  start(runtime?: ScreenBridgeRuntimeOverrides): void;
  call<T = unknown>(
    scope: ScreenScope,
    verb: string,
    args?: Record<string, unknown>,
    options?: ScreenBridgeCallOptions,
  ): Promise<T>;
  slot<T = unknown>(
    scope: ScreenScope,
    verb: string,
    args?: Record<string, unknown>,
    anchor?: SlotAnchor,
    options?: ScreenBridgeCallOptions,
  ): Promise<T>;
  tool<T = unknown>(
    scope: ScreenScope,
    alias: string,
    input?: Record<string, unknown>,
    requestId?: string,
    generationScope?: GenerationScope,
  ): Promise<T>;
  audioCapture(
    scope: ScreenScope,
    options?: ScreenAudioCaptureOptions,
  ): Promise<ScreenAudioCapture>;
  audioSession(
    scope: ScreenScope,
    command: ScreenAudioSessionCommand,
  ): Promise<ScreenAudioSessionSnapshot>;
  getAudioSession(): ScreenAudioSessionSnapshot | null;
  onAudioSession(listener: (state: ScreenAudioSessionSnapshot) => void): () => void;
  asset(scope: ScreenScope, ref: string, options?: { timeoutMs?: number }): Promise<string>;
  assetAdmitted(ref: string): boolean;
  state(data: string): boolean;
  patchRoute(patch: ScreenRoutePatch<Section, View>): void;
  ownLeaveState(): ScreenLeaveHandle | null;
  openDrawer(surface: string, options?: ScreenDrawerOpenOptions): Promise<unknown>;
  closeDrawer(): Promise<unknown>;
  drawerContext<T = unknown>(): Promise<T | null>;
  onWrite(listener: () => void): () => void;
  notifyWritten(): void;
  markUnreachable(message?: string): void;
  retire(cause?: Error | string): void;
  reset(): void;
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

export declare class ScreenBridgeError extends Error {
  name: "ScreenBridgeError";
  status: number;
  code: string | undefined;
  constructor(message: string, status?: number, options?: { code?: string });
}

export declare const REFUSAL_STATUS: Readonly<Record<string, number>>;
export declare const MAX_STATE_BYTES: number;
export declare const MAX_ASSET_BYTES: number;
export declare const DEFAULT_TIMEOUT_MS: number;
export declare const DRAWER_CONTEXT_BYTES: number;
export declare const MAX_PUSH_BYTES: number;
export declare const MAX_REPLY_DATA_BYTES: number;
export declare const MAX_AUDIO_TRANSCRIPT_CHARS: number;
export declare const MAX_AUDIO_UPLOAD_BYTES: number;
export declare const SCREEN_AUDIO_V1: "screen-audio.v1";
export declare const SCREEN_AUDIO_V2: "screen-audio.v2";
export declare const HOST_DRAWER_VERBS: readonly [
  "screen.drawer.open",
  "screen.drawer.close",
  "screen.drawer.context",
];
export declare const ROUTABLE_ID: RegExp;
export declare const ROUTE_RECORD_ID: RegExp;

export declare function readRefusal(value: unknown): { code: string; text: string } | null;
export declare function validateBoot(value: unknown, expectedTag?: string): ScreenBoot;
export declare function readBoot(documentRef?: unknown, expectedTag?: string): ScreenBoot;
export declare function createScreenReceiver(options: ScreenReceiverOptions): ScreenReceiver;
export declare function createScreenBridge<
  Section extends string = string,
  View extends string = "list" | "new",
  Err extends Error = ScreenBridgeError,
>(options: ScreenBridgeOptions<Section, View, Err>): ScreenBridge<Section, View, Err>;
