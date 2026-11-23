/**
 * AOS-711 (CAD-2705): `GET /print/view` — the page agenticos-v2's snapshot
 * renderer opens, as the deciding owner, to make the PDF a member is sent.
 *
 *   /print/view?install=<install_id>&screen=<screen_tag>&section=<section>
 *               [&view=<view_id>][&record=<record_id>][&params=<name=value&...>]
 *
 * It draws exactly that one app view, read-only, under the signed-in viewer's
 * own authority (the same mount the canvas uses), with no board chrome, and
 * sets `window.__printReady = true` once the app frame has reported ready. The
 * renderer waits for that flag; a refused or failed mount never sets it, so a
 * member is never sent a PDF of an error page.
 *
 * Pure parsing, no React: the tests load it directly.
 */
import { paramsOfQuery } from "../home/shareRequest";
import { withParams, type RouteParams } from "../workspace-apps/screen/routeParams";
import type { ScreenRoute } from "../workspace-apps/screen/screenProtocol";

export const PRINT_PATH = "/print/view";
/** After the frame's `ready` there is no "drawn" signal in the screen protocol; give it this long to paint before the flag is set. */
export const PRINT_SETTLE_MS = 1500;

const HOST_ID = /^[A-Za-z0-9_-]{1,128}$/;
const SCREEN_TAG = /^[a-z0-9][a-z0-9-]{0,31}$/;
const FRAME_ID = /^[a-z0-9_-]{1,64}$/;
const VIEW_ID = /^[A-Za-z0-9_-]{1,128}$/;

export interface PrintSpec {
  installId: string;
  tag: string;
  section: string;
  /** The declared view's id, when the request named one; the section and params are what the frame is given. */
  view: string | null;
  recordId: string | null;
  params: RouteParams | undefined;
}

export const isPrintPath = (pathname: string): boolean => pathname === PRINT_PATH;

/** The request, or null when any part of it is not the grammar (nothing is guessed or defaulted). */
export function parsePrintQuery(search: string): PrintSpec | null {
  const q = new URLSearchParams(search);
  const one = (name: string, grammar: RegExp, optional = false): string | null | undefined => {
    const all = q.getAll(name);
    if (all.length === 0) return optional ? null : undefined;
    return all.length === 1 && grammar.test(all[0]!) ? all[0]! : undefined;
  };
  const installId = one("install", HOST_ID);
  const tag = one("screen", SCREEN_TAG);
  const section = one("section", FRAME_ID);
  const view = one("view", VIEW_ID, true);
  const recordId = one("record", FRAME_ID, true);
  if (!installId || !tag || !section || view === undefined || recordId === undefined) return null;
  const rawParams = q.getAll("params");
  if (rawParams.length > 1) return null;
  const params = rawParams.length === 0 ? undefined : paramsOfQuery(rawParams[0]!);
  if (params === null) return null;
  return { installId, tag, section, view, recordId, params };
}

/** Where the frame opens: the one view, as a list route (the canvas's own shape). */
export function printRoute(spec: PrintSpec): ScreenRoute {
  return { section: spec.section, view: "list", record_id: spec.recordId, segment_id: null, ...withParams(spec.params) };
}

/** Why a request cannot be drawn against the installation it names, or null when it can. */
export function printRefusal(spec: PrintSpec, installation: { files: readonly string[]; navigation?: { sections: readonly { id: string }[] } | null }): "screen" | "section" | null {
  const declared = installation.files.filter((f) => /^screens\/[a-z0-9][a-z0-9-]{0,31}\/screens\.json$/.test(f));
  if (declared.length !== 1 || declared[0]!.split("/")[1] !== spec.tag) return "screen";
  const sections = installation.navigation?.sections;
  return sections && !sections.some((s) => s.id === spec.section) ? "section" : null;
}
