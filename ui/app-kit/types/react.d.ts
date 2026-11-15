import type { ScreenBridge, ScreenBridgeError, ScreenBridgeState, ScreenRoutePatch } from "./index.js";

export interface ScreenBridgeStatus<Err extends Error = ScreenBridgeError> {
  readonly status: "loading" | "ready" | "error";
  readonly error: Err | null;
  readonly ready: boolean;
  readonly isLoading: boolean;
  readonly isError: boolean;
}

export interface FrameNavigationAdapterOptions<
  Section extends string = string,
  View extends string = "list" | "new",
> {
  sectionQuery?: string;
  parseHref?: (href: string) => ScreenRoutePatch<Section, View> | null;
}

export interface FrameNavigationOptions {
  replace?: boolean;
}

export interface FrameNavigationAdapter {
  navigate: (href: string, options?: FrameNavigationOptions) => void;
  href: (href: string) => string;
  modifiedClicks: "browser" | "prevent";
}

export declare function useScreenBridge<
  Section extends string = string,
  View extends string = "list" | "new",
  Err extends Error = ScreenBridgeError,
>(bridge: ScreenBridge<Section, View, Err>): ScreenBridgeState<Section, View, Err>;

export declare function useScreenSelector<
  Section extends string = string,
  View extends string = "list" | "new",
  Err extends Error = ScreenBridgeError,
  Selected = unknown,
>(
  bridge: ScreenBridge<Section, View, Err>,
  selector: (state: ScreenBridgeState<Section, View, Err>) => Selected,
  isEqual?: (a: Selected, b: Selected) => boolean,
): Selected;

export declare function useScreenStatus<
  Section extends string = string,
  View extends string = "list" | "new",
  Err extends Error = ScreenBridgeError,
>(bridge: ScreenBridge<Section, View, Err>): ScreenBridgeStatus<Err>;

export declare function createFrameNavigationAdapter<
  Section extends string = string,
  View extends string = "list" | "new",
  Err extends Error = ScreenBridgeError,
>(
  bridge: ScreenBridge<Section, View, Err>,
  options?: FrameNavigationAdapterOptions<Section, View>,
): FrameNavigationAdapter;
