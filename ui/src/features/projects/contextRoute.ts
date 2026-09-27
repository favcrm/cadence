import type { WikiRoute } from "../wiki/Wiki";
import { withinScope } from "../wiki/scope";
import type { Route } from "../../lib/router";

export const projectWikiRoot = (project: string) => `projects/${project}`;
const MODES = ["browse", "edit", "history", "upload", "search"] as const;

export function contextRoute(project: string, href: string): WikiRoute | null {
  const params = new URLSearchParams(href.split("?")[1] ?? "");
  const root = projectWikiRoot(project);
  const file = params.get("file") ?? "";
  const path = file ? `${root}/${file}` : root;
  if (!withinScope(path, root) || file.startsWith("/")) return null;
  const mode = params.get("mode") ?? "browse";
  if (!MODES.includes(mode as typeof MODES[number])) return null;
  return { screen: "wiki", mode: mode as WikiRoute["mode"], path, query: params.get("q") };
}

export function contextHref(project: string, route: WikiRoute, currentHref = ""): string {
  const root = projectWikiRoot(project);
  const path = route.path || root;
  if (!withinScope(path, root)) throw new Error("This path is outside the project's context");
  const params = new URLSearchParams(currentHref.split("?")[1] ?? "");
  for (const key of ["file", "mode", "q", "tab", "view", "project", "run"]) params.delete(key);
  const relative = path === root ? "" : path.slice(root.length + 1);
  if (relative) params.set("file", relative);
  if (route.mode !== "browse") params.set("mode", route.mode);
  if (route.mode === "search" && route.query) params.set("q", route.query);
  const query = params.toString();
  return `/projects/${encodeURIComponent(project)}/context${query ? `?${query}` : ""}`;
}

/** File selection belongs to one project's Context, never another tab or project. */
export function contextNavigationSearch(from: Route, to: Route, search: string): string {
  if (from.screen !== "projects" || from.section !== "context") return search;
  if (to.screen === "projects" && to.section === "context" && to.slug === from.slug) return search;
  const params = new URLSearchParams(search);
  for (const key of ["file", "mode", "q"]) params.delete(key);
  return params.toString();
}
