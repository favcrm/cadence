import { useEffect, useMemo, useState } from "react";
import Link from "../../ui/Link";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import { useTranslate } from "../../lib/locale";
import { sessionHeaders } from "../../lib/sessionKey";
import { notifyAppsChanged } from "../workspace-apps/workspaceApps";
import { useInstallations } from "../workspace-apps/useInstallations";
import { installedAppHref } from "../apps/appViewModel";
import type { Viewer } from "../projects/work";
import "./explorer.css";

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

export type InstallOutcome =
  | { state: "installed"; installId: string; version: string; digest: string; approval: string }
  | { state: "refused"; message: string }
  | { state: "unknown"; message: string }
  | { state: "cancelled" };

const DIGEST = /^sha256:[0-9a-f]{64}$/;

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
  if (typeof row.id !== "string" || !/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(row.id) || typeof row.app !== "string" || typeof row.title !== "string") return null;
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

function textField(row: Record<string, unknown>, key: string): string | null {
  return typeof row[key] === "string" && row[key] ? String(row[key]) : null;
}

/** Confirm, fetch the pinned release with credentials omitted, then the existing upload route. */
export async function installPublicApp(app: PublicApp, signal: AbortSignal): Promise<InstallOutcome> {
  if (!app.installable || !app.version || !app.digest || !DIGEST.test(app.digest)) {
    return { state: "refused", message: "This release is listed for browsing only." };
  }
  const releaseQuery = new URLSearchParams({ version: app.version, digest: app.digest });
  const release = await boardGet(
    `/api/public-apps/${encodeURIComponent(app.id)}/release?${releaseQuery}`,
    signal,
  );
  if (signal.aborted) return { state: "cancelled" };
  if (release.status === 404) return { state: "refused", message: "That release is not published." };
  if (release.status === 409) return { state: "refused", message: "That exact release is not available." };
  if (!release.ok) return { state: "refused", message: "The release could not be read." };
  const body = await release.json() as { files?: unknown; expected_digest?: unknown };
  if (body.expected_digest !== app.digest || !body.files || typeof body.files !== "object" || Array.isArray(body.files)) {
    return { state: "refused", message: "The release did not match the confirmed digest." };
  }
  let uploaded: Response;
  try {
    uploaded = await fetch("/api/app-installations/upload", {
      method: "POST",
      credentials: "same-origin",
      cache: "no-store",
      signal,
      headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
      body: JSON.stringify({
        files: body.files,
        expected_digest: body.expected_digest,
        catalogue: { id: app.id, version: app.version, digest: app.digest },
      }),
    });
  } catch (error) {
    if (signal.aborted) return { state: "unknown", message: "Install was cancelled after it started. Check installed apps before trying again." };
    throw error;
  }
  if (!uploaded.ok) {
    const err = await uploaded.json().catch(() => null) as { error?: string } | null;
    const message = err && typeof err.error === "string" ? err.error : "Install was refused.";
    if (uploaded.status === 401 || uploaded.status === 403) {
      return { state: "refused", message: "Installing needs the host operator. Browsing stays available." };
    }
    return { state: "refused", message };
  }
  const created = await uploaded.json() as Record<string, unknown>;
  const installId = textField(created, "install_id");
  if (!installId) return { state: "unknown", message: "Install finished without an id. Check installed apps; it was not retried." };
  const readback = await fetch(`/api/app-installations/${encodeURIComponent(installId)}`, {
    credentials: "same-origin",
    cache: "no-store",
    headers: { "X-Cadence-Board": "1", ...sessionHeaders() },
  });
  if (!readback.ok) {
    return { state: "unknown", message: "Install was sent but the installed app could not be read back. It was not retried." };
  }
  const row = await readback.json() as Record<string, unknown>;
  const digest = textField(row, "digest");
  const version = textField(row, "version");
  const id = textField(row, "install_id");
  const catalogue = row.catalogue;
  const catalogueRow = catalogue && typeof catalogue === "object" && !Array.isArray(catalogue)
    ? catalogue as Record<string, unknown>
    : null;
  const catalogueMatches = !!catalogueRow
    && textField(catalogueRow, "id") === app.id
    && textField(catalogueRow, "version") === app.version
    && textField(catalogueRow, "digest") === app.digest;
  if (!id || digest !== app.digest || version !== app.version || !catalogueMatches) {
    return { state: "unknown", message: "Readback did not match the confirmed release. It was not retried." };
  }
  notifyAppsChanged();
  return {
    state: "installed",
    installId: id,
    version,
    digest,
    approval: textField(row, "approval") ?? "unapproved",
  };
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

export function PublicDirectory({ viewer }: { viewer: Viewer }) {
  const t = useTranslate();
  const { apps, error, retry } = usePublicList();
  const [query, setQuery] = useState("");
  const [category, setCategory] = useState("all");
  const categories = useMemo(() => {
    const set = new Set((apps ?? []).map((app) => app.listing.category).filter((c): c is string => !!c));
    return [...set].sort((a, b) => a.localeCompare(b));
  }, [apps]);
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
        {t("Browse published apps. Sign in to install; execution approval stays separate.")}
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
        {shown.map((app) => (
          <Link key={app.id} href={`/apps/catalog/${encodeURIComponent(app.id)}`} className="card pubdir-card">
            <span className="pubdir-head">
              <span className="pubdir-tile" aria-hidden="true">{app.title.slice(0, 1)}</span>
              <span className="pubdir-id">
                <strong>{app.title}</strong>
                <small>{app.listing.publisher ?? app.app}</small>
              </span>
              <span className="pubdir-ver">v{app.version}</span>
            </span>
            <span className="pubdir-summary">{app.listing.tagline ?? app.summary}</span>
            <span className="pubdir-meta">
              {app.listing.category && <span className="chip">{app.listing.category}</span>}
              {(app.listing.tags ?? []).map((tag) => <span key={tag} className="chip">{tag}</span>)}
            </span>
          </Link>
        ))}
      </div>
    </section>
  );
}

export function PublicAppDetail({ id, viewer, onResolved }: { id: string; viewer: Viewer; onResolved?: (found: boolean) => void }) {
  const t = useTranslate();
  const [app, setApp] = useState<PublicApp | null | undefined>(undefined);
  const [error, setError] = useState<string | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [phase, setPhase] = useState<"idle" | "working" | InstallOutcome>("idle");
  const [abort, setAbort] = useState<AbortController | null>(null);
  const canInstall = viewer.operator === true && !viewer.readOnly;
  const installedState = useInstallations(canInstall, id);
  useEffect(() => {
    const controller = new AbortController();
    setApp(undefined); setError(null); setPhase("idle"); setConfirming(false);
    loadPublicApp(id, controller.signal)
      .then((row) => {
        if (controller.signal.aborted) return;
        setApp(row);
        onResolved?.(row !== null);
      })
      .catch((e: unknown) => {
        if (controller.signal.aborted) return;
        setError(e instanceof Error ? e.message : "Could not load the app.");
      });
    return () => controller.abort();
  }, [id]);
  const run = () => {
    if (!app) return;
    const controller = new AbortController();
    setAbort(controller);
    setConfirming(false);
    setPhase("working");
    installPublicApp(app, controller.signal)
      .then((outcome) => setPhase(outcome))
      .catch((e: unknown) => setPhase({ state: "refused", message: e instanceof Error ? e.message : "Install was refused." }))
      .finally(() => setAbort(null));
  };
  if (error) return <p className="err" role="alert">{t(error)}</p>;
  if (app === undefined) return <p className="text-label text-ink-400">{t("Loading…")}</p>;
  if (app === null) return <p role="status">{t("No such app.")}</p>;
  const installed = phase !== "idle" && phase !== "working" && phase.state === "installed" ? phase : null;
  const existing = (installedState.list ?? []).find((row) => row.removed == null && row.name === app.app) ?? null;
  const openHref = existing ? installedAppHref(existing.name) : null;
  return (
    <article className="pubdir-detail" aria-label={app.title}>
      <p className="text-label mb-4"><Link href="/apps/explore">{t("All published apps")}</Link></p>
      <div className="pubdir-split">
        <div>
          <header className="pubdir-hero">
            <span className="pubdir-tile" aria-hidden="true">{app.title.slice(0, 1)}</span>
            <div className="pubdir-id">
              <h2 className="text-section font-semibold text-ink-100">{app.title}</h2>
              {app.listing.publisher && <p className="text-label text-ink-500">by {app.listing.publisher}</p>}
              <p className="pubdir-summary">{app.listing.tagline ?? app.summary}</p>
              <span className="pubdir-meta">
                {app.listing.category && <span className="chip">{app.listing.category}</span>}
                {(app.listing.tags ?? []).map((tag) => <span key={tag} className="chip">{tag}</span>)}
                <span className="pubdir-ver">v{app.version}</span>
              </span>
            </div>
          </header>
          <p className="pubdir-about text-secondary text-ink-300">{app.listing.description ?? app.summary}</p>
          <div className="pubdir-sections">
          {app.listing.highlights && app.listing.highlights.length > 0 && (
            <section>
              <h3 className="text-label font-medium text-ink-200">{t("What it can do")}</h3>
              <ul>{app.listing.highlights.map((item) => <li key={item}>{item}</li>)}</ul>
            </section>
          )}
          {app.listing.setup && app.listing.setup.length > 0 && (
            <section>
              <h3 className="text-label font-medium text-ink-200">{t("What you approve")}</h3>
              <ul>{app.listing.setup.map((item) => <li key={item.slot}><b>{item.label}</b> — {item.help}</li>)}</ul>
            </section>
          )}
          {app.listing.stores && app.listing.stores.length > 0 && (
            <section>
              <h3 className="text-label font-medium text-ink-200">{t("Your data")}</h3>
              <ul>
                {app.listing.stores.map((item) => <li key={item}>{item}</li>)}
                {app.listing.personalData && <li><b>{t("Contains personal data.")}</b></li>}
              </ul>
            </section>
          )}
          {app.listing.accessNotes && app.listing.accessNotes.length > 0 && (
            <section>
              <h3 className="text-label font-medium text-ink-200">{t("Access notes")}</h3>
              <ul>{app.listing.accessNotes.map((item) => <li key={item}>{item}</li>)}</ul>
            </section>
          )}
          </div>
        </div>
        <aside className="card pubdir-side">
          <dl className="pubdir-kv">
            <dt>{t("Version")}</dt><dd>{app.version}</dd>
            <dt>{t("Digest")}</dt><dd className="pubdir-digest">{app.digest ?? t("Not published")}</dd>
          </dl>
          {!app.installable && <p className="text-label text-ink-400" role="status">{t("Listed for browsing. This release is not available to install.")}</p>}
          {viewer.operator === null && (
            <a className="btn btn-primary" href={`/__platform/login?to=${encodeURIComponent(window.location.href)}`}>{t("Sign in to install")}</a>
          )}
          {viewer.operator === false && <p className="text-label text-ink-400">{t("Installing needs the host operator. You can keep browsing.")}</p>}
          {canInstall && installedState.error && <p role="alert">{t("Installed apps could not be checked. Retry before installing.")} <Button onClick={installedState.retry}>{t("Retry")}</Button></p>}
          {canInstall && installedState.loading && !existing && phase === "idle" && (
            <p className="text-label text-ink-400" role="status">{t("Checking this board…")}</p>
          )}
          {existing && phase === "idle" && (
            <p className="pubdir-installed" role="status">{t("Installed on this board.")} {existing.version}</p>
          )}
          {existing && openHref && phase === "idle" && (
            <Button className="btn-primary" href={openHref}>{t("Open")}</Button>
          )}
          {canInstall && app.installable && !existing && installedState.list !== null && !installedState.error && !installedState.loading && phase === "idle" && (
            <Button className="btn-primary" onClick={() => setConfirming(true)}>{t("Install")}</Button>
          )}
          {phase === "working" && (
            <p role="status">
              {t("Installing…")}{" "}
              <Button onClick={() => abort?.abort()}>{t("Cancel")}</Button>
            </p>
          )}
          {installed && (
            <p className="pubdir-receipt" role="status">
              {t("Installed")} {installed.installId} {installed.version} {installed.digest}. {t("Execution approval is separate")} ({installed.approval}).
            </p>
          )}
          {phase !== "idle" && phase !== "working" && phase.state === "refused" && <p className="err" role="alert">{phase.message}</p>}
          {phase !== "idle" && phase !== "working" && phase.state === "unknown" && <p className="err" role="alert">{phase.message}</p>}
          {phase !== "idle" && phase !== "working" && phase.state === "cancelled" && <p role="status">{t("Install cancelled.")}</p>}
        </aside>
      </div>
      {confirming && (
        <dialog className="card pubdir-dialog" open aria-labelledby="pubdir-confirm">
          <h3 id="pubdir-confirm">{t("Install {title}?").replace("{title}", app.title)}</h3>
          <p className="text-secondary">{app.version}</p>
          <p className="pubdir-digest">{app.digest}</p>
          <p className="text-label text-ink-500">{t("This installs the package only. It does not approve connections or execution.")}</p>
          <div className="pubdir-actions">
            <Button onClick={() => setConfirming(false)}>{t("Cancel")}</Button>
            <Button className="btn-primary" onClick={run}>{t("Install this release")}</Button>
          </div>
        </dialog>
      )}
    </article>
  );
}
