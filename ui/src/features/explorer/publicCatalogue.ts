/**
 * Public app catalogue (same-origin board proxy).
 * `GET /api/public-apps` → `{ ok: true, data: { apps } }`
 * `GET /api/public-apps/<id>` → `{ ok: true, data: { app } }`
 * A wrong shape fails closed. An empty list is empty. Transport
 * failure is unavailable. A detail 404 is not found.
 */

export interface PublicListing {
  tagline: string;
  category: string;
  publisher: { name: string };
  about: string;
  can: string[];
  icon?: string;
  screenshots?: { caption?: string }[];
  setup?: { label: string; help?: string }[];
  data?: { stores?: string[]; personal?: boolean };
  /** Plain sentences from `listing.access_notes`, when the payload has them. */
  accessNotes?: string[];
}

export interface PublicApp {
  id: string;
  title: string;
  version: string;
  installable: boolean;
  unavailableReason: string | null;
  summary: string;
  listing: PublicListing;
}

export type PublicListResult =
  | { status: "ok"; apps: PublicApp[] }
  | { status: "malformed" }
  | { status: "unavailable" };

export type PublicDetailResult =
  | { status: "ok"; app: PublicApp }
  | { status: "not_found" }
  | { status: "malformed" }
  | { status: "unavailable" };

function str(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function parseListing(value: unknown): PublicListing | null {
  if (!value || typeof value !== "object") return null;
  const row = value as Record<string, unknown>;
  const tagline = str(row.tagline);
  const category = str(row.category);
  const about = str(row.about);
  const publisher = row.publisher;
  const publisherName =
    publisher && typeof publisher === "object" ? str((publisher as Record<string, unknown>).name) : null;
  if (!tagline || !category || !about || !publisherName) return null;
  if (!Array.isArray(row.can) || row.can.some((item) => typeof item !== "string")) return null;
  const listing: PublicListing = {
    tagline,
    category,
    about,
    publisher: { name: publisherName },
    can: row.can as string[],
  };
  if (typeof row.icon === "string") listing.icon = row.icon;
  if (Array.isArray(row.screenshots)) {
    const shots: { caption?: string }[] = [];
    for (const shot of row.screenshots) {
      if (!shot || typeof shot !== "object") return null;
      const caption = (shot as Record<string, unknown>).caption;
      if (caption !== undefined && typeof caption !== "string") return null;
      shots.push(typeof caption === "string" ? { caption } : {});
    }
    listing.screenshots = shots;
  }
  if (Array.isArray(row.setup)) {
    const setup: { label: string; help?: string }[] = [];
    for (const step of row.setup) {
      if (!step || typeof step !== "object") return null;
      const label = str((step as Record<string, unknown>).label);
      if (!label) return null;
      const help = (step as Record<string, unknown>).help;
      if (help !== undefined && typeof help !== "string") return null;
      setup.push(typeof help === "string" ? { label, help } : { label });
    }
    listing.setup = setup;
  }
  if (row.data && typeof row.data === "object") {
    const data = row.data as Record<string, unknown>;
    const parsed: { stores?: string[]; personal?: boolean } = {};
    if (data.stores !== undefined) {
      if (!Array.isArray(data.stores) || data.stores.some((item) => typeof item !== "string")) return null;
      parsed.stores = data.stores as string[];
    }
    if (data.personal !== undefined) {
      if (typeof data.personal !== "boolean") return null;
      parsed.personal = data.personal;
    }
    listing.data = parsed;
  }
  if (row.access_notes && typeof row.access_notes === "object") {
    const notes = Object.values(row.access_notes as Record<string, unknown>);
    if (notes.some((note) => typeof note !== "string")) return null;
    listing.accessNotes = notes as string[];
  }
  return listing;
}

export function parsePublicApp(value: unknown): PublicApp | null {
  if (!value || typeof value !== "object") return null;
  const row = value as Record<string, unknown>;
  const id = str(row.id);
  const title = str(row.title);
  const version = str(row.version);
  const summary = str(row.summary);
  if (!id || !title || version === null || summary === null) return null;
  if (typeof row.installable !== "boolean") return null;
  if (row.unavailableReason !== null && typeof row.unavailableReason !== "string") return null;
  if (!("unavailableReason" in row)) return null;
  const listing = parseListing(row.listing);
  if (!listing) return null;
  return {
    id,
    title,
    version,
    summary,
    installable: row.installable,
    unavailableReason: row.unavailableReason,
    listing,
  };
}

function notFoundBody(body: unknown): boolean {
  if (!body || typeof body !== "object") return false;
  const error = (body as Record<string, unknown>).error;
  if (!error || typeof error !== "object") return false;
  return (error as Record<string, unknown>).code === "not_found";
}

async function readBody(resp: Response): Promise<unknown | undefined> {
  const text = await resp.text();
  if (!text) return undefined;
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return undefined;
  }
}

export async function fetchPublicApps(signal: AbortSignal): Promise<PublicListResult> {
  let resp: Response;
  try {
    resp = await fetch("/api/public-apps", { signal, headers: { Accept: "application/json" } });
  } catch (error) {
    if (signal.aborted) throw error;
    return { status: "unavailable" };
  }
  const body = await readBody(resp);
  if (signal.aborted) throw new DOMException("aborted", "AbortError");
  if (!resp.ok || body === undefined) return { status: "unavailable" };
  if (!body || typeof body !== "object" || (body as Record<string, unknown>).ok !== true) {
    return { status: "malformed" };
  }
  const data = (body as Record<string, unknown>).data;
  if (!data || typeof data !== "object" || !Array.isArray((data as Record<string, unknown>).apps)) {
    return { status: "malformed" };
  }
  const apps: PublicApp[] = [];
  for (const row of (data as Record<string, unknown>).apps as unknown[]) {
    const app = parsePublicApp(row);
    if (!app) return { status: "malformed" };
    apps.push(app);
  }
  return { status: "ok", apps };
}

export async function fetchPublicApp(id: string, signal: AbortSignal): Promise<PublicDetailResult> {
  let resp: Response;
  try {
    resp = await fetch(`/api/public-apps/${encodeURIComponent(id)}`, {
      signal,
      headers: { Accept: "application/json" },
    });
  } catch (error) {
    if (signal.aborted) throw error;
    return { status: "unavailable" };
  }
  const body = await readBody(resp);
  if (signal.aborted) throw new DOMException("aborted", "AbortError");
  if (resp.status === 404 || notFoundBody(body)) return { status: "not_found" };
  if (!resp.ok || body === undefined) return { status: "unavailable" };
  if (!body || typeof body !== "object" || (body as Record<string, unknown>).ok !== true) {
    return { status: "malformed" };
  }
  const data = (body as Record<string, unknown>).data;
  const app = data && typeof data === "object" ? parsePublicApp((data as Record<string, unknown>).app) : null;
  if (!app) return { status: "malformed" };
  return { status: "ok", app };
}

/** A catalogue reason code is not a price and not an install promise. */
export function unavailableCopy(app: PublicApp): string | null {
  if (app.installable) return null;
  if (app.unavailableReason === "artifact_unavailable") {
    return "This release has no installable package right now.";
  }
  return "This release isn't available to install from here.";
}

/** Full-document platform login for the page the visitor is on. */
export function platformLoginHref(pageHref: string): string {
  const absolute = new URL(pageHref, window.location.origin).href;
  return `/__platform/login?to=${encodeURIComponent(absolute)}`;
}
