import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import Link from "../../ui/Link";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import { useLocale } from "../../lib/locale";
import { useHref, navigate } from "../../lib/useLocation";
import {
  appExplorer,
  compareSemverVersions,
  workspaceApps,
  APPS_CHANGED_EVENT,
  type HomeInstallation,
  type Installation,
} from "../workspace-apps/workspaceApps";
import { extractAccessNoteEntries, isPlainJsonObject, localizedAppCategory, localizedAppSummary, localizedAppTagline, localizedAppTitle, parseAppDisplayI18n, parseListingDisplayI18n, pickLocaleEntry, type AppDisplayI18n, type ListingDisplayLocale } from "../workspace-apps/appLocale";
import { loadFailureMessage, loadWithBackoff } from "../workspace-apps/installationsLoad";
import { onPrivateReadsPaused, onPrivateSessionReset, privateReadsAllowed, privateSessionGeneration } from "../../lib/privateSession";
import { sessionHeaders } from "../../lib/sessionKey";
import { installationAppHref } from "../apps/appViewModel";
import { AppTile, AppTileSkeletons } from "../apps/AppTile";
import AppCover from "./AppCover";
import ReleasePanel, { InstalledActions } from "./releasePanel";
import type { Viewer } from "../projects/work";
import "./publicDirectory.css";

export type PublicSetupItem = {
  slot: string;
  label: string;
  help: string;
};

export type PublicListing = {
  tagline?: string;
  category?: string;
  tags?: string[];
  publisher?: string;
  description?: string;
  highlights?: string[];
  setup?: PublicSetupItem[];
  stores?: string[];
  personalData?: boolean;
  accessNotes?: string[];
  i18n?: Record<string, ListingDisplayLocale>;
};

export type PublicApp = {
  id: string;
  app: string;
  title: string;
  summary: string;
  version: string;
  listing: PublicListing;
  i18n?: AppDisplayI18n | null;
  digest: string | null;
  installable: boolean;
};

/** One row of the closed `/versions` document. `installable` and a null
 * `unavailableReason` must agree — the same pair the daemon requires. */
export type PublicRelease = {
  version: string;
  digest: string;
  installable: boolean;
  unavailableReason: string | null;
  release_notes: string | null;
};

/** The served versions document (exactly the four keys the daemon admits). */
export type PublicVersions = {
  app_id: string;
  app: string;
  recommended_version: string | null;
  releases: PublicRelease[];
};
/** The install rows a viewer may know: the operator's verified list, or a
 * member's readable `/api/app-home` rows (never privileged fields). */
import { canOpenInstall, type InstallLifecycleState } from "./installAccess";

export type InstallRef = Pick<
  Installation,
  "install_id" | "name" | "title" | "removed" | "catalogue"
> & Partial<Pick<Installation, "version" | "digest" | "catalog_generation" | "project_link" | "i18n" | "listing">> & {
  project?: string;
  /** The tenant's lifecycle word; only an active row can open its app route. */
  state?: InstallLifecycleState;
};

const DIGEST = /^sha256:[0-9a-f]{64}$/;
/** The proxy's admitted id and version token charsets — a stricter client
 * pattern would reject releases the closed catalogue serves. */
const CATALOG_ID = /^[A-Za-z0-9._-]{1,64}$/;
const VERSION_TOKEN = /^[A-Za-z0-9._+-]{1,40}$/;
const UNAVAILABLE_REASONS = new Set(["digest_unverified", "artifact_unavailable", "app_disabled", "release_disabled", "host_incompatible"]);

const FORBIDDEN_PUBLIC_KEYS = new Set(["cost", "price", "pricing", "artifact", "path", "__proto__", "prototype", "constructor"]);
const MAX_PUBLIC_SCAN_DEPTH = 32;
const MAX_PUBLIC_SCAN_NODES = 4096;

function hasUnsafePublicStructure(value: unknown, depth = 0, state = { nodes: 0 }): boolean {
  if (depth > MAX_PUBLIC_SCAN_DEPTH) return true;
  if (value === null || typeof value !== "object") return false;
  if (++state.nodes > MAX_PUBLIC_SCAN_NODES) return true;
  if (Array.isArray(value)) return value.some((item) => hasUnsafePublicStructure(item, depth + 1, state));
  if (!isPlainJsonObject(value)) return true;
  return Object.keys(value).some((k) => FORBIDDEN_PUBLIC_KEYS.has(k) || hasUnsafePublicStructure(value[k], depth + 1, state));
}

function listingOf(value: unknown): PublicListing | null {
  if (value === undefined || value === null) return {};
  if (!isPlainJsonObject(value)) return null;
  const row = value;
  const rawList = Array.isArray(row.can) ? row.can : Array.isArray(row.highlights) ? row.highlights : undefined;
  const highlights = rawList
    ? rawList.filter((item): item is string => typeof item === "string")
    : undefined;
  const tags = Array.isArray(row.tags)
    ? row.tags.filter((item): item is string => typeof item === "string")
    : undefined;
  const pubObj = isPlainJsonObject(row.publisher) ? row.publisher.name : row.publisher;
  const publisher = typeof pubObj === "string" && pubObj ? pubObj : undefined;
  const description = typeof row.about === "string" && row.about
    ? row.about
    : typeof row.description === "string" && row.description
      ? row.description
      : undefined;
  const tagline = typeof row.tagline === "string" && row.tagline ? row.tagline : undefined;
  const setup = Array.isArray(row.setup)
    ? row.setup
        .filter((s): s is Record<string, unknown> => isPlainJsonObject(s))
        .filter((s) => typeof s.label === "string" && typeof s.help === "string")
        .map((s) => ({ slot: typeof s.slot === "string" ? s.slot : s.label as string, label: s.label as string, help: s.help as string }))
    : undefined;
  const dataObj = isPlainJsonObject(row.data) ? row.data : null;
  const stores = dataObj && Array.isArray(dataObj.stores)
    ? dataObj.stores.filter((item): item is string => typeof item === "string")
    : undefined;
  const personalData = dataObj && typeof dataObj.personal === "boolean" ? dataObj.personal : undefined;
  const accessObj = isPlainJsonObject(row.access_notes) ? row.access_notes : null;
  const accessNotes = accessObj
    ? Object.values(accessObj).filter((item): item is string => typeof item === "string" && !!item)
    : undefined;
  let i18n: Record<string, ListingDisplayLocale> | undefined;
  if (row.i18n !== undefined && row.i18n !== null) {
    const parsedListingI18n = parseListingDisplayI18n(row.i18n, { wire: true });
    if (!parsedListingI18n) return null;
    if (Object.keys(parsedListingI18n).length > 0) i18n = parsedListingI18n;
  }
  return {
    tagline,
    category: typeof row.category === "string" ? row.category : undefined,
    tags,
    publisher,
    description,
    highlights,
    setup,
    stores,
    personalData,
    accessNotes,
    ...(i18n ? { i18n } : {}),
  };
}

export function parsePublicApp(value: unknown): PublicApp | null {
  if (!isPlainJsonObject(value)) return null;
  const row = value;
  if (typeof row.installable !== "boolean") return null;
  if (typeof row.id !== "string" || !CATALOG_ID.test(row.id) || row.id.startsWith(".") || row.id.includes("..") || typeof row.app !== "string" || typeof row.title !== "string") return null;
  if (typeof row.summary !== "string" || typeof row.version !== "string") return null;
  if (row.digest !== null && (typeof row.digest !== "string" || !DIGEST.test(row.digest))) return null;
  if (row.installable && row.digest === null) return null;
  if (hasUnsafePublicStructure(row)) return null;
  let parsedI18n: AppDisplayI18n | undefined;
  if (row.i18n !== undefined && row.i18n !== null) {
    const parsed = parseAppDisplayI18n(row.i18n, { wire: true });
    if (!parsed) return null;
    if (Object.keys(parsed).length > 0) parsedI18n = parsed;
  }
  const listing = listingOf(row.listing);
  if (!listing) return null;
  return {
    id: row.id,
    app: row.app,
    title: row.title,
    summary: row.summary,
    version: row.version,
    listing,
    ...(parsedI18n ? { i18n: parsedI18n } : {}),
    digest: row.digest as string | null,
    installable: row.installable,
  };
}

export function parsePublicList(body: unknown): PublicApp[] {
  if (!body || typeof body !== "object" || !("ok" in body) || body.ok !== true || !("data" in body)) throw new Error("The catalogue returned an invalid response.");
  const data = body.data;
  if (!data || typeof data !== "object" || !("apps" in data) || !Array.isArray(data.apps)) throw new Error("The catalogue returned an invalid response.");
  const apps: PublicApp[] = [];
  const ids = new Set<string>();
  for (const value of data.apps) {
    const app = parsePublicApp(value);
    if (!app) throw new Error("The catalogue returned an invalid app.");
    if (ids.has(app.id)) throw new Error("The catalogue returned duplicate apps.");
    ids.add(app.id);
    apps.push(app);
  }
  return apps;
}

function releaseOf(value: unknown): PublicRelease {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("The catalogue returned an invalid release.");
  const row = value as Record<string, unknown>;
  if (typeof row.version !== "string" || !VERSION_TOKEN.test(row.version)) throw new Error("The catalogue returned an invalid release.");
  if (typeof row.digest !== "string" || !DIGEST.test(row.digest)) throw new Error("The catalogue returned an invalid release.");
  if (typeof row.installable !== "boolean") throw new Error("The catalogue returned an invalid release.");
  const reason = row.unavailableReason;
  if (reason !== null && (typeof reason !== "string" || !UNAVAILABLE_REASONS.has(reason))) throw new Error("The catalogue returned an invalid release.");
  if (row.installable !== (reason === null)) throw new Error("The catalogue returned an invalid release.");
  const notes = row.release_notes;
  if (notes !== null && typeof notes !== "string") throw new Error("The catalogue returned an invalid release.");
  return {
    version: row.version,
    digest: row.digest,
    installable: row.installable,
    unavailableReason: reason as string | null,
    release_notes: notes as string | null,
  };
}

/** The served shape is exactly `app_id`/`app`/`recommended_version`/`releases`
 * — the same closed document the daemon's eligibility check admits. */
export function parsePublicVersions(body: unknown, id: string, app: string): PublicVersions {
  if (!body || typeof body !== "object" || Array.isArray(body)) throw new Error("The catalogue returned invalid versions.");
  const row = body as Record<string, unknown>;
  if (Object.keys(row).length !== 4) throw new Error("The catalogue returned invalid versions.");
  if (row.app_id !== id || row.app !== app) throw new Error("The catalogue returned versions for another app.");
  if (row.recommended_version !== null && (typeof row.recommended_version !== "string" || !VERSION_TOKEN.test(row.recommended_version))) throw new Error("The catalogue returned invalid versions.");
  if (!Array.isArray(row.releases) || row.releases.length > 500) throw new Error("The catalogue returned invalid versions.");
  const releases = row.releases.map(releaseOf);
  const names = new Set(releases.map((item) => item.version));
  if (names.size !== releases.length) throw new Error("The catalogue returned conflicting releases.");
  if (row.recommended_version !== null && !names.has(row.recommended_version)) throw new Error("The catalogue returned invalid versions.");
  return {
    app_id: row.app_id,
    app: row.app,
    recommended_version: row.recommended_version as string | null,
    releases,
  };
}

async function boardGet(path: string, signal: AbortSignal): Promise<Response> {
  try {
    return await fetch(path, {
      credentials: "same-origin",
      cache: "no-store",
      signal,
      headers: { "X-Cadence-Board": "1", ...sessionHeaders() },
    });
  } catch (error) {
    if (signal.aborted) throw error;
    throw new Error("The public directory didn't load.");
  }
}

export async function loadPublicApps(signal: AbortSignal): Promise<PublicApp[]> {
  const response = await boardGet("/api/public-apps", signal);
  if (!response.ok) throw new Error("The public directory didn't load.");
  return parsePublicList(await response.json());
}

export async function loadPublicApp(id: string, signal: AbortSignal): Promise<PublicApp | null> {
  const response = await boardGet(`/api/public-apps/${encodeURIComponent(id)}`, signal);
  if (response.status === 404) return null;
  if (!response.ok) throw new Error("The public app didn't load.");
  const body = await response.json() as { ok?: unknown; data?: { app?: unknown } };
  const app = body.ok === true ? parsePublicApp(body.data?.app) : null;
  if (!app || app.id !== id) throw new Error("The catalogue returned an invalid app.");
  return app;
}

export async function loadPublicVersions(app: PublicApp, signal: AbortSignal): Promise<PublicVersions> {
  const response = await boardGet(`/api/public-apps/${encodeURIComponent(app.id)}/versions`, signal);
  if (!response.ok) throw new Error("The published versions didn't load.");
  return parsePublicVersions(await response.json(), app.id, app.app);
}

/** The verified operator list, or the member-readable home rows — never a
 * privileged read for a member, and refreshable for readback. */
function useBoardInstalls(viewer: Viewer): {
  rows: InstallRef[] | null;
  error: string | null;
  loading: boolean;
  retry: () => void;
  refresh: () => Promise<InstallRef[] | null>;
  accept: (row: Installation) => void;
} {
  const operator = viewer.operator === true;
  const signedIn = viewer.operator !== null;
  const [rows, setRows] = useState<InstallRef[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [tick, setTick] = useState(0);
  const generation = useRef(0);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; generation.current += 1; };
  }, []);
  useEffect(() => {
    const clear = () => {
      generation.current += 1;
      setRows(null); setError(null); setLoading(false);
      setTick((n) => n + 1);
    };
    const stopReset = onPrivateSessionReset(clear);
    const stopPause = onPrivateReadsPaused(clear);
    return () => { stopReset(); stopPause(); };
  }, []);

  // A committed install elsewhere in this session re-reads the private
  // rows — never while reads are paused, never for a guest, and never in a
  // session older than the listener (reset re-subscribes on the tick it
  // bumps; each settled read is guarded by `generation` on commit).
  useEffect(() => {
    if (!signedIn) return;
    const epoch = privateSessionGeneration();
    const bump = () => {
      if (!privateReadsAllowed() || privateSessionGeneration() !== epoch) return;
      setTick((n) => n + 1);
    };
    window.addEventListener(APPS_CHANGED_EVENT, bump);
    return () => window.removeEventListener(APPS_CHANGED_EVENT, bump);
  }, [signedIn, tick]);

  const read = useCallback((signal: AbortSignal, shared: boolean): Promise<InstallRef[]> => {
    const fromOperator = (list: Installation[]): InstallRef[] => list
      .map((row) => ({
        install_id: row.install_id, name: row.name, title: row.title, version: row.version,
        digest: row.digest, catalog_generation: row.catalog_generation, project_link: row.project_link,
        state: row.state, removed: row.removed, catalogue: row.catalogue, project: row.project_link ?? undefined,
        i18n: row.i18n, listing: row.listing,
      }));
    const fromHome = (home: HomeInstallation[]): InstallRef[] => home
      .map((row) => ({
        install_id: row.install_id, name: row.name, title: row.title, version: row.version,
        project_link: row.project ?? "", removed: null, catalogue: row.catalogue ?? null, project: row.project,
        state: row.attention?.state === "removed" ? "removed" : row.attention?.state === "off" ? "disabled" : undefined,
        i18n: row.i18n, listing: row.listing,
      }));
    const once = () => operator
      ? workspaceApps.installations(signal, shared).then(fromOperator)
      : appExplorer.home(signal, shared ? (viewer.readOnly ? "viewer" : "member") : undefined).then((home) => {
        if (!Array.isArray(home?.installations)) throw new Error("Installed apps could not be checked.");
        return fromHome(home.installations);
      });
    return loadWithBackoff(once, { signal });
  }, [operator, viewer.readOnly]);

  useEffect(() => {
    if (!signedIn || !privateReadsAllowed()) { setRows(null); setError(null); setLoading(false); return; }
    const controller = new AbortController();
    const gen = ++generation.current;
    setLoading(true);
    read(controller.signal, true).then(
      (list) => { if (!controller.signal.aborted && generation.current === gen) { setRows(list); setError(null); } },
      (cause: unknown) => { if (!controller.signal.aborted && generation.current === gen) setError(loadFailureMessage(cause)); },
    ).finally(() => { if (!controller.signal.aborted && generation.current === gen) setLoading(false); });
    return () => controller.abort();
  }, [signedIn, tick, read, viewer.access, viewer.readOnly]);

  const refresh = useCallback(async (): Promise<InstallRef[] | null> => {
    const controller = new AbortController();
    const gen = ++generation.current;
    setLoading(true);
    try {
      const list = await read(controller.signal, false);
      if (!mounted.current || generation.current !== gen) return null;
      setRows(list);
      setError(null);
      return list;
    } catch (cause: unknown) {
      if (mounted.current && generation.current === gen) setError(loadFailureMessage(cause));
      return null;
    } finally {
      if (mounted.current && generation.current === gen) setLoading(false);
    }
  }, [read]);
  const accept = (row: Installation) => {
    const confirmed: InstallRef = {
      install_id: row.install_id, name: row.name, title: row.title, version: row.version,
      digest: row.digest, catalog_generation: row.catalog_generation, project_link: row.project_link,
      state: row.state, removed: row.removed, catalogue: row.catalogue, project: row.project_link ?? undefined,
      i18n: row.i18n, listing: row.listing,
    };
    setRows((current) => [...(current ?? []).filter((item) => item.install_id !== row.install_id), confirmed]);
    setError(null);
  };
  return { rows, error, loading, retry: () => setTick((n) => n + 1), refresh, accept };
}


function usePublicList() {
  const [apps, setApps] = useState<PublicApp[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  useEffect(() => {
    const controller = new AbortController();
    setError(null);
    loadPublicApps(controller.signal)
      .then((rows) => { if (!controller.signal.aborted) setApps(rows); })
      .catch((e: unknown) => {
        if (!controller.signal.aborted) setError(e instanceof Error ? e.message : "The public directory didn't load.");
      });
    return () => controller.abort();
  }, [tick]);
  return { apps, error, retry: () => setTick((n) => n + 1) };
}

/** `/apps/catalog/<id>` — with the installation the card stands for when the
 * workspace names it unambiguously. */
function catalogDetailHref(appId: string, installId: string | null): string {
  const base = `/apps/catalog/${encodeURIComponent(appId)}`;
  return installId ? `${base}?installation=${encodeURIComponent(installId)}` : base;
}

export function PublicDirectory({ viewer, afterListings }: {
  viewer: Viewer;
  afterListings?: (publishedIds: ReadonlySet<string>) => ReactNode;
}) {
  const { locale, t } = useLocale();
  const { apps, error, retry } = usePublicList();
  const installs = useBoardInstalls(viewer);
  const [query, setQuery] = useState("");
  const [category, setCategory] = useState("all");
  const [availability, setAvailability] = useState("all");
  const known = installs.rows !== null && installs.error === null;
  const availabilityUnknown = availability !== "all" && !known;
  const publishedIds = useMemo(() => new Set(apps?.map((app) => app.id)), [apps]);
  const categories = useMemo(() => {
    const set = new Set((apps ?? []).map((app) => app.listing.category).filter((c): c is string => !!c));
    return [...set].sort((a, b) => a.localeCompare(b));
  }, [apps]);
  const installsByName = useMemo(() => {
    const map = new Map<string, InstallRef[]>();
    for (const row of installs.rows ?? []) {
      if (row.state === "removed" || row.removed != null) continue;
      const list = map.get(row.name) ?? [];
      list.push(row);
      map.set(row.name, list);
    }
    return map;
  }, [installs.rows]);
  const localizedById = useMemo(() => new Map(
    (apps ?? []).map((app) => [
      app.id,
      {
        title: localizedAppTitle({ title: app.title, i18n: app.i18n, listing: app.listing }, locale, t),
        tagline:
          localizedAppTagline({ tagline: app.listing.tagline, i18n: app.i18n, listing: app.listing }, locale, t) ||
          localizedAppSummary({ summary: app.summary, i18n: app.i18n, listing: app.listing }, locale, t),
      },
    ]),
  ), [apps, locale, t]);
  const needle = query.trim().toLowerCase();
  const shown = availabilityUnknown ? [] : (apps ?? []).filter((app) => {
    if (category !== "all" && app.listing.category !== category) return false;
    if (availability !== "all" && (availability === "installed") !== installsByName.has(app.app)) return false;
    if (!needle) return true;
    const localized = localizedById.get(app.id);
    const hay = [
      app.title,
      localized?.title,
      app.summary,
      localized?.tagline,
      app.app,
      app.listing.tagline,
      app.listing.publisher,
      ...(app.listing.tags ?? []),
    ]
      .filter(Boolean)
      .join(" ")
      .toLowerCase();
    return hay.includes(needle);
  });
  const canInstall = viewer.operator === true && !viewer.readOnly;
  return (
    <section className="pubdir" aria-label={t("Public apps")}>
      <p className="text-secondary text-ink-400 mb-3">
        {t("Find apps for your workspace.")}
      </p>
      {!canInstall && viewer.operator !== null && (
        <p className="text-label text-ink-400 mb-3" role="status">{t("You can browse. Installing needs the host operator.")}</p>
      )}
      {viewer.operator === null && (
        <p className="mb-3"><a className="btn btn-primary btn-sm" href={`/__platform/login?to=${encodeURIComponent(window.location.href)}`}>{t("Sign in to install")}</a></p>
      )}
      <div className="pubdir-tools">
        <input className="field" value={query} onChange={(e) => setQuery(e.target.value)} placeholder={t("Search apps")} aria-label={t("Search apps")} />
        <div className="pubdir-category">
          <Select
            full
            value={category}
            onChange={setCategory}
            aria-label={t("Category")}
            options={[
              { value: "all", label: t("All categories") },
              ...categories.map((item) => ({ value: item, label: localizedAppCategory(item, locale) })),
            ]}
          />
        </div>
        <div className="pubdir-category">
          <Select full aria-label={t("Installation status")} value={availability} onChange={setAvailability} options={[
            { value: "all", label: t("All apps") },
            { value: "installed", label: t("Installed") },
            { value: "available", label: t("Not installed") },
          ]} />
        </div>
      </div>
      {apps && !availabilityUnknown && (
        <p className="pubdir-count text-label text-ink-500">
          {shown.length === 1 ? t("1 app") : t("{count} apps").replace("{count}", String(shown.length))}
        </p>
      )}
      {error && (
        <div className="err card px-4 py-3 mb-4 text-label" role="alert">
          {t(error)} <Button onClick={retry}>{t("Try again")}</Button>
        </div>
      )}
      {apps === null && !error && <AppTileSkeletons label={t("Loading published apps…")} />}
      {apps !== null && availabilityUnknown && (
        <p className="pubdir-empty" role="status">{t(installs.error ? "Installation unavailable" : "Checking this board…")}</p>
      )}
      {apps !== null && !availabilityUnknown && shown.length === 0 && !error && <p className="pubdir-empty">{t("No published apps match.")}</p>}
      {shown.length > 0 && <div className="apps-grid">
        {shown.map((app) => {
          const rows = installsByName.get(app.app) ?? [];
          const only = rows.length === 1 ? rows[0] : null;
          const installed = known && rows.length > 0;
          const hasUpdate = known && !installs.loading && only !== null &&
            canOpenInstall(only.state, only.removed) && app.installable &&
            typeof only.version === "string" && compareSemverVersions(app.version, only.version) === 1;
          const localized = localizedById.get(app.id);
          return <AppTile key={app.id}
            name={app.app}
            title={localized?.title ?? app.title}
            summary={localized?.tagline ?? app.listing.tagline ?? app.summary}
            version={`v${app.version}`}
            status={t(!known ? "Not checked" : hasUpdate ? "Update available" : installed ? "Installed" : "Not installed")}
            tone={hasUpdate ? "info" : installed ? "" : "muted"}
            href={catalogDetailHref(app.id, only?.install_id ?? null)}
          />;
        })}
      </div>}
      {(apps !== null || error !== null) && afterListings?.(publishedIds)}
    </section>
  );
}

/** A requested release or the directory's explicit recommendation.
 * Missing metadata never chooses a different release for the user. */
function selectedRelease(versions: PublicVersions | null, requested: string | null): { release: PublicRelease | null; missing: boolean } {
  if (versions === null) return { release: null, missing: false };
  const releases = versions.releases;
  const byVersion = (version: string | null) => releases.find((item) => item.version === version) ?? null;
  if (requested !== null) return { release: byVersion(requested), missing: byVersion(requested) === null };
  if (versions.recommended_version) {
    const rec = byVersion(versions.recommended_version);
    if (rec?.installable) return { release: rec, missing: false };
  }
  return { release: null, missing: false };
}

/** What an install row claims for its catalogue locator: an id, nothing, or
 * an unreadable shape. A malformed locator is neither a match nor proof the
 * id is absent — it defers the miss verdict rather than inventing one. */
type RowCatalogue = { kind: "id"; id: string } | { kind: "none" } | { kind: "malformed" };
function rowCatalogue(row: InstallRef): RowCatalogue {
  const locator = row.catalogue;
  if (locator === null || locator === undefined) return { kind: "none" };
  if (typeof locator !== "object" || Array.isArray(locator) || typeof locator.id !== "string") return { kind: "malformed" };
  return { kind: "id", id: locator.id };
}

/** The catalogue record a failed public lookup may still stand for: only
 * installations whose recorded `catalogue.id` names this page's id (an exact
 * `?installation=` picks inside them — never another install). It carries no
 * published metadata, so no release/version check is offered. */
function LocalInstallPanel({ id, appError, onRetry, rows, installsError, onRetryInstalls, recordsUnreadable, installationParam, operator }: {
  id: string;
  appError: string | null;
  onRetry: () => void;
  rows: InstallRef[] | null;
  installsError: string | null;
  onRetryInstalls: () => void;
  recordsUnreadable: boolean;
  installationParam: string | null;
  operator: boolean;
}) {
  const { locale, t } = useLocale();
  const catalogued = (rows ?? []).filter((row) => {
    const claim = rowCatalogue(row);
    return claim.kind === "id" && claim.id === id;
  });
  const named = installationParam !== null ? catalogued.find((row) => row.install_id === installationParam) ?? null : null;
  const installParamMissing = installationParam !== null && rows !== null && named === null;
  // Only a claimant with a readable install_id may be chosen or linked.
  const chooser = installationParam === null && catalogued.length > 1
    ? catalogued.filter((row) => typeof row.install_id === "string")
    : null;
  const install = installationParam !== null ? named : catalogued.length === 1 ? catalogued[0] : null;
  // A claimant without a readable install_id stays a fact row — no Open,
  // Manage or chooser link can name it.
  const addressable = install !== null && typeof install.install_id === "string" ? install : null;
  const removed = addressable !== null && (addressable.state === "removed" ||
    (addressable.removed != null && addressable.removed !== 0));
  // A disabled or removed install has no openable route: offering Open there
  // sends the reader to "Unknown installation".
  const openable = addressable !== null && canOpenInstall(addressable.state, addressable.removed);
  const openHref = addressable && openable ? installationAppHref(rows, addressable.install_id) : null;
  const titleSource = install ?? catalogued[0];
  const title = titleSource ? localizedAppTitle(titleSource, locale, t) : id;
  return (
    <article className="pub-detail" aria-label={title}>
      <p className="pub-back"><Link href="/apps/explore">{t("All published apps")}</Link></p>
      {appError !== null ? (
        <div className="err card px-4 py-3 mb-4 text-label" role="alert">
          {t(appError)} <Button onClick={onRetry}>{t("Try again")}</Button>
        </div>
      ) : (
        <p className="pub-note" role="status">{t("This app is not listed in the public directory.")}</p>
      )}
      {catalogued.length > 0 && (
        <aside className="pub-install-panel" aria-label={t("Installed app")}>
          <h2 className="pub-panel-title">{title}</h2>
          {installParamMissing && (
            <p className="pub-note" role="status">
              {t("The linked installation is not in this workspace.")}
            </p>
          )}
          {chooser && (
            <div className="pub-note" role="status">
              <p>
                {t("This app has more than one installation. Choose which one to review — none is picked for you.")}
              </p>
              <ul className="pub-chooser">
                {chooser.map((row) => (
                  <li key={row.install_id}>
                    <Link
                      replace
                      href={`/apps/catalog/${encodeURIComponent(id)}?${new URLSearchParams({ installation: row.install_id })}`}
                    >
                      {localizedAppTitle(row, locale, t)} · {row.version ?? "—"}
                      {row.project ? ` · ${row.project}` : ""}
                    </Link>
                  </li>
                ))}
              </ul>
            </div>
          )}
          {addressable && (
            <>
              <dl className="pub-facts">
                <div>
                  <dt>{t("Installed")}</dt>
                  <dd>{addressable.version ?? "—"}</dd>
                </div>
              </dl>
              {removed ? (
                <p className="pub-note" role="status">{t("This installation was removed and cannot be opened or changed here.")}</p>
              ) : (
                <div className="pub-actions">
                  <InstalledActions install={addressable} openHref={openHref} operator={operator} />
                </div>
              )}
            </>
          )}
          <p className="pub-caption">
            {t(removed ? "This removed installation is retained in the workspace." : "The installed copy stays usable. Published versions and update checks return when the catalogue answers.")}
          </p>
        </aside>
      )}
      {recordsUnreadable && (
        <p className="pub-note" role="status">
          {t("Some installed-app records could not be fully read — check again before assuming this app is absent.")}{" "}
          <Button size="sm" onClick={onRetryInstalls}>{t("Retry")}</Button>
        </p>
      )}
      {installsError && (
        <p className="pub-note" role="alert">
          {t("Installed apps could not be checked. Retry before installing.")}{" "}
          <Button size="sm" onClick={onRetryInstalls}>{t("Retry")}</Button>
        </p>
      )}
    </article>
  );
}

export function CatalogDetailSkeleton({ label }: { label: string }) {
  const { t } = useLocale();
  return <article className="pub-detail">
    <div className="pub-back"><Link href="/apps/explore">{t("All published apps")}</Link></div>
    <p className="sr-only" role="status">{label}</p>
    <div aria-hidden="true">
      <header className="pub-heading pub-skeleton-heading">
        <span className="app-skeleton" /><span className="app-skeleton" /><span className="app-skeleton" /><span className="app-skeleton" />
      </header>
      <div className="pub-layout">
        <div className="app-skeleton pub-skeleton-cover" />
        <div className="card pub-install-panel pub-skeleton-workspace">
          <span className="app-skeleton" /><span className="app-skeleton" /><span className="app-skeleton" />
        </div>
      </div>
    </div>
  </article>;
}

export function PublicAppDetail({ id, viewer, onResolved }: { id: string; viewer: Viewer; onResolved?: (found: boolean) => void }) {
  const { locale, t } = useLocale();
  const [app, setApp] = useState<PublicApp | null | undefined>(undefined);
  const [appError, setAppError] = useState<string | null>(null);
  const [appTick, setAppTick] = useState(0);
  const [versions, setVersions] = useState<PublicVersions | null>(null);
  const [versionsError, setVersionsError] = useState<string | null>(null);
  const [versionsTick, setVersionsTick] = useState(0);
  const installs = useBoardInstalls(viewer);
  const href = useHref();
  const params = new URLSearchParams(href.includes("?") ? href.slice(href.indexOf("?") + 1) : "");
  const versionParam = params.get("version");
  const installationParam = params.get("installation");
  const signedIn = viewer.operator !== null;
  const operator = viewer.operator === true && !viewer.readOnly;

  // A new catalogue id restarts the lookup; `appTick` is only the explicit
  // retry and keeps the failed answer (with any local install) on screen.
  useEffect(() => {
    setApp(undefined); setAppError(null);
    setVersions(null); setVersionsError(null);
  }, [id]);

  useEffect(() => {
    const controller = new AbortController();
    loadPublicApp(id, controller.signal)
      .then((row) => {
        if (controller.signal.aborted) return;
        setAppError(null);
        setApp(row);
        // A miss is not yet a verdict: a signed-in workspace may still hold
        // an install recorded against this catalogue id. The effect below
        // hands off to the retired-catalog detail only when it does not.
        if (row !== null) onResolved?.(true);
      })
      .catch((e: unknown) => {
        if (controller.signal.aborted) return;
        setAppError(e instanceof Error ? e.message : "Could not load the app.");
      });
    return () => controller.abort();
  }, [id, appTick]);

  useEffect(() => {
    if (!app) return;
    setVersions(null); setVersionsError(null);
    const controller = new AbortController();
    loadPublicVersions(app, controller.signal)
      .then((doc) => { if (!controller.signal.aborted) setVersions(doc); })
      .catch((e: unknown) => { if (!controller.signal.aborted) setVersionsError(e instanceof Error ? e.message : "The published versions didn't load."); });
    return () => controller.abort();
  }, [app, versionsTick]);

  /** Installations recorded against this catalogue id — the only records a
   * failed lookup may stand for. An install that never claimed this
   * catalogue entry is not offered in its place. */
  const catalogued = useMemo(
    () => (installs.rows ?? []).filter((row) => {
      const claim = rowCatalogue(row);
      return claim.kind === "id" && claim.id === id;
    }),
    [installs.rows, id],
  );
  /** Any unreadable locator defers the miss verdict: a malformed record
   * could be this app, so it is never counted as confirmed absence. */
  const recordsUnreadable = useMemo(
    () => (installs.rows ?? []).some((row) => rowCatalogue(row).kind === "malformed"),
    [installs.rows],
  );
  /** The workspace answer is still unread (first private read in flight). */
  const installsPending = signedIn && installs.rows === null && installs.error === null;
  /** Absence is confirmed only by a complete, readable private answer — a
   * guest's empty set, an errored read or a malformed record never counts. */
  const confirmedMiss =
    app === null &&
    !installsPending &&
    installs.error === null &&
    catalogued.length === 0 &&
    (!signedIn || (installs.rows !== null && !recordsUnreadable));

  // A public 404 hands off to the retired-catalog detail only once the
  // workspace's own records are settled and none claims this catalogue id;
  // a matching install keeps the page instead of being swallowed.
  useEffect(() => {
    if (confirmedMiss) onResolved?.(false);
  }, [confirmedMiss, onResolved]);

  if (app === null && installsPending) return <CatalogDetailSkeleton label={t("Checking this board…")} />;
  if (app === null || appError !== null) {
    if (confirmedMiss) return <p role="status">{t("No such app.")}</p>;
    return (
      <LocalInstallPanel
        id={id}
        appError={appError}
        onRetry={() => setAppTick((n) => n + 1)}
        rows={installs.rows}
        installsError={installs.error}
        onRetryInstalls={installs.retry}
        recordsUnreadable={recordsUnreadable}
        installationParam={installationParam}
        operator={operator}
      />
    );
  }
  if (app === undefined) return <CatalogDetailSkeleton label={t("Loading…")} />;

  // The same name, or the catalogue id recorded on the install — an
  // installation that claims neither is never treated as this app.
  const matching = (installs.rows ?? []).filter((row) => {
    if (row.name === app.app) return true;
    const claim = rowCatalogue(row);
    return claim.kind === "id" && claim.id === app.id;
  });
  const named = installationParam !== null ? matching.find((row) => row.install_id === installationParam) ?? null : null;
  const installParamMissing = installationParam !== null && installs.rows !== null && named === null;
  const chooser = installationParam === null && matching.length > 1 ? matching : null;
  const install = installationParam !== null ? named : matching.length === 1 ? matching[0] : null;
  const openHref =
    install && canOpenInstall(install.state, install.removed)
      ? installationAppHref(installs.rows, install.install_id)
      : null;
  const { release, missing } = selectedRelease(versions, versionParam);

  const selectVersion = (version: string) => {
    if (version === versionParam) return;
    const next = new URLSearchParams(params);
    next.set("version", version);
    navigate(`/apps/catalog/${encodeURIComponent(app.id)}?${next}`, { replace: true });
  };

  const locListing = pickLocaleEntry(app.listing.i18n, locale);
  const displayTitle = localizedAppTitle(
    { title: app.title, i18n: app.i18n, listing: app.listing },
    locale,
    t,
  );
  const displaySummary = localizedAppSummary(
    { summary: app.summary, i18n: app.i18n, listing: app.listing },
    locale,
    t,
  );
  const displayIntro = localizedAppTagline(
    { tagline: app.listing.tagline, i18n: app.i18n, listing: app.listing },
    locale,
    t,
  ) || displaySummary;
  const displayDescription = locListing?.about ?? app.listing.description ?? displaySummary;
  const highlights = locListing?.can ?? app.listing.highlights ?? [];
  const accessNotes = extractAccessNoteEntries(locListing?.access_notes, app.listing.accessNotes);
  return (
    <article className="pub-detail" aria-label={displayTitle}>
      <p className="pub-back"><Link href="/apps/explore">{t("All published apps")}</Link></p>
      <header className="pub-heading">
        {app.listing.category && <p className="pub-category">{localizedAppCategory(app.listing.category, locale)}</p>}
        <h1 id="pub-title" tabIndex={-1}>{displayTitle}</h1>
        <p className="pub-intro">{displayIntro}</p>
        <p className="pub-publisher">
          {app.listing.publisher ? t("By {publisher}").replace("{publisher}", () => app.listing.publisher!) : app.app}
          <span className="pub-unverified">{t("Publisher not verified")}</span>
        </p>
      </header>
      <div className="pub-layout">
        <div className="pub-story">
          <figure className="pub-cover">
            <AppCover name={app.app} title={displayTitle} category={app.listing.category} />
            <figcaption>{t("Illustrative app cover")}</figcaption>
          </figure>
          <section className="pub-section">
            <h2>{t("About this app")}</h2>
            <p>{displayDescription}</p>
          </section>
          {highlights.length > 0 && (
            <section className="pub-section">
              <h2>{t("What you can do")}</h2>
              <ul className="pub-features">
                {highlights.map((item, idx) => (
                  <li key={`${idx}:${item}`}>
                    <svg aria-hidden="true" viewBox="0 0 20 20"><path d="m4 10 4 4 8-9" /></svg>
                    <span>{item}</span>
                  </li>
                ))}
              </ul>
            </section>
          )}
          {release?.release_notes && (
            <section className="pub-section">
              <h2>{t("What’s new")}<span className="pub-release-label">{release.version}</span></h2>
              <p>{release.release_notes}</p>
            </section>
          )}
          {app.listing.setup && app.listing.setup.length > 0 && (
            <section className="pub-section">
              <h2>{t("What you approve")}</h2>
              <ul className="pub-plain">{app.listing.setup.map((item) => <li key={item.slot}><b>{item.label}</b> — {item.help}</li>)}</ul>
            </section>
          )}
          {app.listing.stores && app.listing.stores.length > 0 && (
            <section className="pub-section">
              <h2>{t("Your data")}</h2>
              <ul className="pub-plain">
                {app.listing.stores.map((item) => <li key={item}>{item}</li>)}
                {app.listing.personalData && <li><b>{t("Contains personal data.")}</b></li>}
              </ul>
            </section>
          )}
          {accessNotes && accessNotes.length > 0 && (
            <section className="pub-section">
              <h2>{t("Access notes")}</h2>
              <ul className="pub-plain">{accessNotes.map(([slotKey, item]) => <li key={slotKey}>{item}</li>)}</ul>
            </section>
          )}
        </div>
        <ReleasePanel
          app={app}
          release={release}
          versions={versions}
          versionsError={versionsError}
          onRetryVersions={() => setVersionsTick((n) => n + 1)}
          install={install}
          installsReady={installs.rows !== null && !installs.loading}
          installsError={installs.error}
          onRetryInstalls={installs.retry}
          refreshInstalls={installs.refresh}
          onApplied={installs.accept}
          openHref={openHref}
          operator={operator}
          signedIn={viewer.operator !== null}
          chooser={chooser}
          installParamMissing={installParamMissing}
          onSelectVersion={selectVersion}
        />
      </div>
      {missing && versionParam && (
        <p className="pub-note" role="status">{t("{version} is not a published release here.").replace("{version}", versionParam)}</p>
      )}
    </article>
  );
}
