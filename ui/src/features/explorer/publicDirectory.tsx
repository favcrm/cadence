import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import Link from "../../ui/Link";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import { useTranslate } from "../../lib/locale";
import { useHref, navigate } from "../../lib/useLocation";
import { appExplorer, workspaceApps, type HomeInstallation, type Installation } from "../workspace-apps/workspaceApps";
import { loadFailureMessage, loadWithBackoff } from "../workspace-apps/installationsLoad";
import { onPrivateReadsPaused, onPrivateSessionReset, privateReadsAllowed } from "../../lib/privateSession";
import { installationAppHref } from "../apps/appViewModel";
import AppCover from "./AppCover";
import ReleasePanel from "./releasePanel";
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
};

export type PublicApp = {
  id: string;
  app: string;
  title: string;
  summary: string;
  version: string;
  listing: PublicListing;
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
export type InstallRef = Pick<
  Installation,
  "install_id" | "name" | "title" | "removed" | "catalogue"
> & Partial<Pick<Installation, "version" | "digest" | "catalog_generation" | "project_link">> & { project?: string };

const DIGEST = /^sha256:[0-9a-f]{64}$/;
/** The proxy's admitted id and version token charsets — a stricter client
 * pattern would reject releases the closed catalogue serves. */
const CATALOG_ID = /^[A-Za-z0-9._-]{1,64}$/;
const VERSION_TOKEN = /^[A-Za-z0-9._+-]{1,40}$/;
const UNAVAILABLE_REASONS = new Set(["digest_unverified", "artifact_unavailable", "app_disabled", "release_disabled"]);

function listingOf(value: unknown): PublicListing {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  const row = value as Record<string, unknown>;
  const rawList = Array.isArray(row.can) ? row.can : Array.isArray(row.highlights) ? row.highlights : undefined;
  const highlights = rawList
    ? rawList.filter((item): item is string => typeof item === "string")
    : undefined;
  const tags = Array.isArray(row.tags)
    ? row.tags.filter((item): item is string => typeof item === "string")
    : undefined;
  const pubObj = row.publisher && typeof row.publisher === "object" && !Array.isArray(row.publisher)
    ? (row.publisher as Record<string, unknown>).name
    : row.publisher;
  const publisher = typeof pubObj === "string" && pubObj ? pubObj : undefined;
  const description = typeof row.about === "string" && row.about
    ? row.about
    : typeof row.description === "string" && row.description
      ? row.description
      : undefined;
  const tagline = typeof row.tagline === "string" && row.tagline ? row.tagline : undefined;
  const setup = Array.isArray(row.setup)
    ? row.setup
        .filter((s): s is Record<string, unknown> => !!s && typeof s === "object" && !Array.isArray(s))
        .filter((s) => typeof s.label === "string" && typeof s.help === "string")
        .map((s) => ({ slot: typeof s.slot === "string" ? s.slot : s.label as string, label: s.label as string, help: s.help as string }))
    : undefined;
  const dataObj = row.data && typeof row.data === "object" && !Array.isArray(row.data)
    ? (row.data as Record<string, unknown>)
    : null;
  const stores = dataObj && Array.isArray(dataObj.stores)
    ? dataObj.stores.filter((item): item is string => typeof item === "string")
    : undefined;
  const personalData = dataObj && typeof dataObj.personal === "boolean" ? dataObj.personal : undefined;
  const accessObj = row.access_notes && typeof row.access_notes === "object" && !Array.isArray(row.access_notes)
    ? (row.access_notes as Record<string, unknown>)
    : null;
  const accessNotes = accessObj
    ? Object.values(accessObj).filter((item): item is string => typeof item === "string" && !!item)
    : undefined;
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
  };
}

export function parsePublicApp(value: unknown): PublicApp | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const row = value as Record<string, unknown>;
  if (typeof row.installable !== "boolean") return null;
  if (typeof row.id !== "string" || !CATALOG_ID.test(row.id) || row.id.startsWith(".") || row.id.includes("..") || typeof row.app !== "string" || typeof row.title !== "string") return null;
  if (typeof row.summary !== "string" || typeof row.version !== "string") return null;
  if (row.digest !== null && (typeof row.digest !== "string" || !DIGEST.test(row.digest))) return null;
  if (row.installable && row.digest === null) return null;
  if ("price" in row || "artifact" in row || "path" in row) return null;
  return {
    id: row.id,
    app: row.app,
    title: row.title,
    summary: row.summary,
    version: row.version,
    listing: listingOf(row.listing),
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
      headers: { "X-Cadence-Board": "1" },
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

  const read = useCallback((signal: AbortSignal): Promise<InstallRef[]> => {
    const fromOperator = (list: Installation[]): InstallRef[] => list
      .filter((row) => row.removed == null)
      .map((row) => ({
        install_id: row.install_id, name: row.name, title: row.title, version: row.version,
        digest: row.digest, catalog_generation: row.catalog_generation, project_link: row.project_link,
        removed: row.removed, catalogue: row.catalogue, project: row.project_link ?? undefined,
      }));
    const fromHome = (home: HomeInstallation[]): InstallRef[] => home
      .filter((row) => row.attention?.state !== "removed")
      .map((row) => ({
        install_id: row.install_id, name: row.name, title: row.title, version: row.version,
        project_link: row.project ?? "", removed: null, catalogue: row.catalogue ?? null, project: row.project,
      }));
    const once = () => operator
      ? workspaceApps.installations(signal).then(fromOperator)
      : appExplorer.home(signal).then((home) => {
        if (!Array.isArray(home?.installations)) throw new Error("Installed apps could not be checked.");
        return fromHome(home.installations);
      });
    return loadWithBackoff(once, { signal });
  }, [operator]);

  useEffect(() => {
    if (!signedIn || !privateReadsAllowed()) { setRows(null); setError(null); setLoading(false); return; }
    const controller = new AbortController();
    const gen = ++generation.current;
    setLoading(true);
    read(controller.signal).then(
      (list) => { if (!controller.signal.aborted && generation.current === gen) { setRows(list); setError(null); } },
      (cause: unknown) => { if (!controller.signal.aborted && generation.current === gen) setError(loadFailureMessage(cause)); },
    ).finally(() => { if (!controller.signal.aborted && generation.current === gen) setLoading(false); });
    return () => controller.abort();
  }, [signedIn, tick, read, viewer.access, viewer.readOnly]);

  const refresh = useCallback(async (): Promise<InstallRef[] | null> => {
    const controller = new AbortController();
    const gen = ++generation.current;
    const list = await read(controller.signal);
    if (!mounted.current || generation.current !== gen) return list;
    setRows(list);
    setError(null);
    return list;
  }, [read]);
  const accept = (row: Installation) => {
    const confirmed: InstallRef = {
      install_id: row.install_id, name: row.name, title: row.title, version: row.version,
      digest: row.digest, catalog_generation: row.catalog_generation, project_link: row.project_link,
      removed: row.removed, catalogue: row.catalogue, project: row.project_link ?? undefined,
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

export function PublicDirectory({ viewer }: { viewer: Viewer }) {
  const t = useTranslate();
  const { apps, error, retry } = usePublicList();
  const installs = useBoardInstalls(viewer);
  const [query, setQuery] = useState("");
  const [category, setCategory] = useState("all");
  const categories = useMemo(() => {
    const set = new Set((apps ?? []).map((app) => app.listing.category).filter((c): c is string => !!c));
    return [...set].sort((a, b) => a.localeCompare(b));
  }, [apps]);
  const installsByName = useMemo(() => {
    const map = new Map<string, InstallRef[]>();
    for (const row of installs.rows ?? []) {
      const list = map.get(row.name) ?? [];
      list.push(row);
      map.set(row.name, list);
    }
    return map;
  }, [installs.rows]);
  const shown = (apps ?? []).filter((app) => {
    const hay = [
      app.title,
      app.summary,
      app.app,
      app.listing.tagline,
      app.listing.publisher,
      ...(app.listing.tags ?? []),
    ]
      .filter(Boolean)
      .join(" ")
      .toLowerCase();
    if (query && !hay.includes(query.trim().toLowerCase())) return false;
    if (category !== "all" && app.listing.category !== category) return false;
    return true;
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
              ...categories.map((item) => ({ value: item, label: item })),
            ]}
          />
        </div>
      </div>
      {apps && !error && (
        <p className="pubdir-count text-label text-ink-500">
          {shown.length === 1 ? t("1 app") : t("{count} apps").replace("{count}", String(shown.length))}
        </p>
      )}
      {error && (
        <div className="err card px-4 py-3 mb-4 text-label" role="alert">
          {t(error)} <Button onClick={retry}>{t("Try again")}</Button>
        </div>
      )}
      {apps === null && !error && <p className="text-label text-ink-400">{t("Loading published apps…")}</p>}
      {apps && shown.length === 0 && !error && <p className="pubdir-empty">{t("No published apps match.")}</p>}
      <div className="pubdir-grid">
        {shown.map((app) => {
          const rows = installsByName.get(app.app) ?? [];
          const only = rows.length === 1 ? rows[0] : null;
          const known = installs.rows !== null && !installs.loading && !installs.error;
          return (
            <article className="pub-card" key={app.id}>
              <Link href={catalogDetailHref(app.id, only?.install_id ?? null)} className="pub-card-link" aria-labelledby={`pub-name-${app.id}`} aria-describedby={`pub-state-${app.id}`}>
                <AppCover name={app.app} title={app.title} category={app.listing.category} />
                <span className="pub-card-copy">
                  <span className="pub-card-category">{app.listing.category ?? ""}</span>
                  <span className="pub-card-title" id={`pub-name-${app.id}`}>{app.title}</span>
                  <span className="pub-card-summary">{app.listing.tagline ?? app.summary}</span>
                  {app.listing.publisher && <span className="pub-card-publisher">{app.listing.publisher}</span>}
                  <span className="pub-card-footer">
                    <span>v{app.version}</span>
                    <span id={`pub-state-${app.id}`} className="pub-status" {...(known && rows.length ? { "data-state": "statusInstalled" } : {})}>
                      {!known ? t("Not checked") : rows.length ? t("Installed") : t("Not installed")}
                    </span>
                  </span>
                </span>
              </Link>
            </article>
          );
        })}
      </div>
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

export function PublicAppDetail({ id, viewer, onResolved }: { id: string; viewer: Viewer; onResolved?: (found: boolean) => void }) {
  const t = useTranslate();
  const [app, setApp] = useState<PublicApp | null | undefined>(undefined);
  const [appError, setAppError] = useState<string | null>(null);
  const [versions, setVersions] = useState<PublicVersions | null>(null);
  const [versionsError, setVersionsError] = useState<string | null>(null);
  const [versionsTick, setVersionsTick] = useState(0);
  const installs = useBoardInstalls(viewer);
  const href = useHref();
  const params = new URLSearchParams(href.includes("?") ? href.slice(href.indexOf("?") + 1) : "");
  const versionParam = params.get("version");
  const installationParam = params.get("installation");
  const operator = viewer.operator === true && !viewer.readOnly;

  useEffect(() => {
    const controller = new AbortController();
    setApp(undefined); setAppError(null); setVersions(null); setVersionsError(null);
    loadPublicApp(id, controller.signal)
      .then((row) => {
        if (controller.signal.aborted) return;
        setApp(row);
        onResolved?.(row !== null);
      })
      .catch((e: unknown) => {
        if (controller.signal.aborted) return;
        setAppError(e instanceof Error ? e.message : "Could not load the app.");
      });
    return () => controller.abort();
  }, [id]);

  useEffect(() => {
    if (!app) return;
    setVersions(null); setVersionsError(null);
    const controller = new AbortController();
    loadPublicVersions(app, controller.signal)
      .then((doc) => { if (!controller.signal.aborted) setVersions(doc); })
      .catch((e: unknown) => { if (!controller.signal.aborted) setVersionsError(e instanceof Error ? e.message : "The published versions didn't load."); });
    return () => controller.abort();
  }, [app, versionsTick]);

  if (appError) return <p className="err" role="alert">{t(appError)}</p>;
  if (app === undefined) return <p className="text-label text-ink-400">{t("Loading…")}</p>;
  if (app === null) return <p role="status">{t("No such app.")}</p>;

  const matching = (installs.rows ?? []).filter((row) => row.name === app.app);
  const named = installationParam !== null ? matching.find((row) => row.install_id === installationParam) ?? null : null;
  const installParamMissing = installationParam !== null && installs.rows !== null && named === null;
  const chooser = installationParam === null && matching.length > 1 ? matching : null;
  const install = installationParam !== null ? named : matching.length === 1 ? matching[0] : null;
  const openHref = install ? installationAppHref(installs.rows, install.install_id) : null;
  const { release, missing } = selectedRelease(versions, versionParam);

  const selectVersion = (version: string) => {
    if (version === versionParam) return;
    const next = new URLSearchParams(params);
    next.set("version", version);
    navigate(`/apps/catalog/${encodeURIComponent(app.id)}?${next}`, { replace: true });
  };

  const highlights = app.listing.highlights ?? [];
  return (
    <article className="pub-detail" aria-label={app.title}>
      <p className="pub-back"><Link href="/apps/explore">{t("All published apps")}</Link></p>
      <header className="pub-heading">
        {app.listing.category && <p className="pub-category">{app.listing.category}</p>}
        <h1 id="pub-title" tabIndex={-1}>{app.title}</h1>
        <p className="pub-intro">{app.listing.tagline ?? app.summary}</p>
        <p className="pub-publisher">
          {app.listing.publisher ? t("By {publisher}").replace("{publisher}", app.listing.publisher) : app.app}
          <span className="pub-unverified">{t("Publisher not verified")}</span>
        </p>
      </header>
      <div className="pub-layout">
        <div className="pub-story">
          <figure className="pub-cover">
            <AppCover name={app.app} title={app.title} category={app.listing.category} />
            <figcaption>{t("Illustrative app cover")}</figcaption>
          </figure>
          <section className="pub-section">
            <h2>{t("About this app")}</h2>
            <p>{app.listing.description ?? app.summary}</p>
          </section>
          {highlights.length > 0 && (
            <section className="pub-section">
              <h2>{t("What you can do")}</h2>
              <ul className="pub-features">
                {highlights.map((item) => (
                  <li key={item}>
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
          {app.listing.accessNotes && app.listing.accessNotes.length > 0 && (
            <section className="pub-section">
              <h2>{t("Access notes")}</h2>
              <ul className="pub-plain">{app.listing.accessNotes.map((item) => <li key={item}>{item}</li>)}</ul>
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
