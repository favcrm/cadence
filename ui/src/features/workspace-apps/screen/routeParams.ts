/** CAD-2555: the params of a declared app view as a route carries them. No dependencies: the card, the canvas and the frame protocol all read it. */
/** CAD-2555 — a declared view's params: at most 6, names `[a-z_]{1,24}`, values plain text of 1-200 characters. */
export type RouteParams = Record<string, string>;
const PARAM_NAME = /^[a-z_]{1,24}$/;
const NO_CONTROLS = /^[^\u0000-\u001f\u007f-\u009f\u2028\u2029]*$/;
const ROUTE_PARAMS_MAX = 6;
export function routeParams(value: unknown): value is RouteParams {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return false;
  const entries = Object.entries(value);
  return entries.length <= ROUTE_PARAMS_MAX && entries.every(([name, v]) =>
    PARAM_NAME.test(name) && typeof v === "string" && v.length >= 1 && v.length <= 200 && NO_CONTROLS.test(v));
}
/** The board URL carries a route's params as `p.<name>=<value>`. */
const URL_PARAM_PREFIX = "p.";
/** The params a board URL names; absent when it names none (or a bad set: all dropped, never part-read). */
export function paramsFromQuery(q: URLSearchParams): RouteParams | undefined {
  const pairs: [string, string][] = [];
  for (const [key, value] of q) if (key.startsWith(URL_PARAM_PREFIX)) pairs.push([key.slice(URL_PARAM_PREFIX.length), value]);
  const found = Object.fromEntries(pairs); // own keys only: `__proto__` is a legal name
  return pairs.length > 0 && routeParams(found) ? found : undefined;
}
/** Rewrites the query's `p.*` keys to exactly `params` (none clears them). */
export function setQueryParams(q: URLSearchParams, params: RouteParams): void {
  for (const key of [...q.keys()]) if (key.startsWith(URL_PARAM_PREFIX)) q.delete(key);
  for (const [name, value] of Object.entries(params)) q.set(URL_PARAM_PREFIX + name, value);
}
/** `params` when it names any, else no key at all: a route without params has its old shape. */
export const withParams = (params: RouteParams | undefined): { params?: RouteParams } =>
  params && Object.keys(params).length > 0 ? { params } : {};
