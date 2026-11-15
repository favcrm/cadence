import { useRef, useSyncExternalStore } from "react";

export function useScreenBridge(bridge) {
  return useSyncExternalStore(bridge.subscribe, bridge.getState, bridge.getState);
}

export function useScreenSelector(bridge, selector, isEqual = Object.is) {
  const cacheRef = useRef({
    initialized: false,
    bridge: undefined,
    state: undefined,
    selector: undefined,
    selected: undefined,
  });
  const getSelected = () => {
    const state = bridge.getState();
    const cache = cacheRef.current;
    if (cache.initialized && cache.bridge === bridge && cache.state === state && cache.selector === selector) {
      return cache.selected;
    }
    const nextSelected = selector(state);
    if (cache.initialized && cache.bridge === bridge && isEqual(cache.selected, nextSelected)) {
      cache.state = state;
      cache.selector = selector;
      return cache.selected;
    }
    cache.initialized = true;
    cache.bridge = bridge;
    cache.state = state;
    cache.selector = selector;
    cache.selected = nextSelected;
    return nextSelected;
  };
  return useSyncExternalStore(bridge.subscribe, getSelected, getSelected);
}

const statusEqual = (a, b) =>
  a.status === b.status &&
  a.error === b.error &&
  a.ready === b.ready &&
  a.isLoading === b.isLoading &&
  a.isError === b.isError;

export function useScreenStatus(bridge) {
  return useScreenSelector(
    bridge,
    (state) => ({
      status: state.status,
      error: state.error,
      ready: state.status === "ready",
      isLoading: state.status === "loading",
      isError: state.status === "error",
    }),
    statusEqual,
  );
}

export function createFrameNavigationAdapter(bridge, options = {}) {
  const parseHref = typeof options.parseHref === "function" ? options.parseHref : null;
  const sectionQuery =
    typeof options.sectionQuery === "string" && options.sectionQuery ? options.sectionQuery : "section";

  const resolvePatch = (to) => {
    if (parseHref) return parseHref(to);
    const raw = typeof to === "string" ? to : "";
    let url;
    try {
      url = new URL(raw, "https://frame.local");
    } catch {
      return null;
    }
    const patch = {};
    const section = url.searchParams.get(sectionQuery);
    if (section !== null && section !== "") patch.section = section;
    const view = url.searchParams.get("view");
    if (view !== null && view !== "") patch.view = view;
    for (const key of ["record_id", "segment_id"]) {
      const val = url.searchParams.get(key);
      if (val !== null) patch[key] = val || null;
    }
    return Object.keys(patch).length > 0 ? patch : null;
  };

  return Object.freeze({
    navigate(to, navOptions) {
      const patch = resolvePatch(to);
      if (patch && typeof bridge?.patchRoute === "function") {
        bridge.patchRoute(typeof navOptions?.replace === "boolean" ? { ...patch, replace: navOptions.replace } : patch);
      }
    },
    href(to) {
      return typeof to === "string" && to ? to : "#";
    },
    modifiedClicks: "prevent",
  });
}
