import { useEffect, useMemo, useState } from "react";
import Link from "../../ui/Link";
import Button from "../../ui/Button";
import { useTranslate } from "../../lib/locale";
import { sessionHeaders } from "../../lib/sessionKey";
import { notifyAppsChanged } from "../workspace-apps/workspaceApps";
import type { Viewer } from "../projects/work";
import "./explorer.css";

export type PublicListing = {
  category?: string;
  publisher?: string;
  description?: string;
  highlights?: string[];
};

export type PublicApp = {
  id: string;
  app: string;
  title: string;
  summary: string;
  version: string;
  listing: PublicListing;
  digest: string;
  installable: true;
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
  const highlights = Array.isArray(row.highlights)
    ? row.highlights.filter((item): item is string => typeof item === "string")
    : undefined;
  return {
    category: typeof row.category === "string" ? row.category : undefined,
    publisher: typeof row.publisher === "string" ? row.publisher : undefined,
    description: typeof row.description === "string" ? row.description : undefined,
    highlights,
  };
}

export function parsePublicApp(value: unknown): PublicApp | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const row = value as Record<string, unknown>;
  if (row.installable !== true) return null;
  if (typeof row.id !== "string" || typeof row.app !== "string" || typeof row.title !== "string") return null;
  if (typeof row.summary !== "string" || typeof row.version !== "string" || !DIGEST.test(String(row.digest))) return null;
  if ("price" in row || "artifact" in row || "path" in row) return null;
  return {
    id: row.id,
    app: row.app,
    title: row.title,
    summary: row.summary,
    version: row.version,
    listing: listingOf(row.listing),
    digest: String(row.digest),
    installable: true,
  };
}

export function parsePublicList(body: unknown): PublicApp[] {
  if (!body || typeof body !== "object") return [];
  const data = (body as { data?: { apps?: unknown } }).data;
  const apps = data && Array.isArray(data.apps) ? data.apps : [];
  return apps.map(parsePublicApp).filter((app): app is PublicApp => app !== null);
}

async function catalogOrigin(signal: AbortSignal): Promise<string> {
  const response = await fetch("/api/public-directory-config", {
    credentials: "same-origin",
    cache: "no-store",
    signal,
    headers: { "X-Cadence-Board": "1" },
  });
  if (!response.ok) throw new Error("This host has no public app directory.");
  const body = await response.json() as { origin?: unknown };
  if (typeof body.origin !== "string" || !body.origin) throw new Error("This host has no public app directory.");
  return body.origin.replace(/\/$/, "");
}

async function publicGet(url: string, signal: AbortSignal): Promise<Response> {
  return fetch(url, { credentials: "omit", cache: "no-store", signal });
}

export async function loadPublicApps(signal: AbortSignal): Promise<PublicApp[]> {
  const origin = await catalogOrigin(signal);
  const response = await publicGet(`${origin}/public/apps`, signal);
  if (!response.ok) throw new Error("The public directory didn't load.");
  return parsePublicList(await response.json());
}

export async function loadPublicApp(id: string, signal: AbortSignal): Promise<PublicApp | null> {
  const origin = await catalogOrigin(signal);
  const response = await publicGet(`${origin}/public/apps/${encodeURIComponent(id)}`, signal);
  if (response.status === 404) return null;
  if (!response.ok) throw new Error("The public app didn't load.");
  const body = await response.json() as { data?: { app?: unknown } };
  return parsePublicApp(body?.data?.app);
}

function textField(row: Record<string, unknown>, key: string): string | null {
  return typeof row[key] === "string" && row[key] ? String(row[key]) : null;
}

/** Confirm, fetch the pinned release with credentials omitted, then the existing upload route. */
export async function installPublicApp(app: PublicApp, signal: AbortSignal): Promise<InstallOutcome> {
  const origin = await catalogOrigin(signal);
  const release = await publicGet(
    `${origin}/public/apps/${encodeURIComponent(app.id)}/release?digest=${encodeURIComponent(app.digest)}`,
    signal,
  );
  if (signal.aborted) return { state: "cancelled" };
  if (release.status === 404) return { state: "refused", message: "That release is not published." };
  if (release.status === 409) return { state: "refused", message: "That release digest is no longer current." };
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
      body: JSON.stringify({ files: body.files, expected_digest: body.expected_digest }),
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
  if (!id || digest !== app.digest || !version) {
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
    return [...set];
  }, [apps]);
  const shown = (apps ?? []).filter((app) => {
    const hay = `${app.title} ${app.summary} ${app.app}`.toLowerCase();
    if (query && !hay.includes(query.trim().toLowerCase())) return false;
    if (category !== "all" && app.listing.category !== category) return false;
    return true;
  });
  const canInstall = viewer.operator === true && !viewer.readOnly;
  return (
    <section className="pubdir" aria-label={t("Public apps")}>
      <p className="text-secondary text-ink-400 mb-3">
        {t("Published free apps. Installing uses this host's operator and does not approve the app to run.")}
      </p>
      {!canInstall && viewer.operator !== null && (
        <p className="text-label text-ink-400 mb-3" role="status">{t("You can browse. Installing needs the host operator.")}</p>
      )}
      <div className="pubdir-tools">
        <input className="field" value={query} onChange={(e) => setQuery(e.target.value)} placeholder={t("Search apps")} aria-label={t("Search apps")} />
        <select className="field" value={category} onChange={(e) => setCategory(e.target.value)} aria-label={t("Category")}>
          <option value="all">{t("All categories")}</option>
          {categories.map((item) => <option key={item} value={item}>{item}</option>)}
        </select>
      </div>
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
            <span className="pubdir-tile" aria-hidden="true">{app.title.slice(0, 1)}</span>
            <span>
              <strong>{app.title}</strong>
              <small className="block text-ink-500">{app.listing.publisher ?? app.app}</small>
            </span>
            <span className="pubdir-summary">{app.summary}</span>
            <span className="pubdir-meta">
              {app.listing.category && <span className="chip">{app.listing.category}</span>}
              <span className="pubdir-ver">{app.version}</span>
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
        onResolved?.(false);
      });
    return () => controller.abort();
  }, [id]);
  const canInstall = viewer.operator === true && !viewer.readOnly;
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
  if (app === null) return null;
  const installed = phase !== "idle" && phase !== "working" && phase.state === "installed" ? phase : null;
  return (
    <article className="pubdir-detail" aria-label={app.title}>
      <p className="text-label mb-2"><Link href="/apps/explore">{t("All published apps")}</Link></p>
      <div className="pubdir-split">
        <div>
          <h2 className="text-section font-semibold text-ink-100">{app.title}</h2>
          <p className="text-secondary text-ink-300">{app.listing.description ?? app.summary}</p>
          {app.listing.highlights && (
            <ul>{app.listing.highlights.map((item) => <li key={item}>{item}</li>)}</ul>
          )}
        </div>
        <aside className="card pubdir-side">
          <dl className="pubdir-kv">
            <dt>{t("Version")}</dt><dd>{app.version}</dd>
            <dt>{t("Digest")}</dt><dd className="pubdir-digest">{app.digest}</dd>
          </dl>
          {!canInstall && <p className="text-label text-ink-400" role="status">{t("You can browse. Installing needs the host operator.")}</p>}
          {canInstall && phase === "idle" && (
            <Button className="btn-primary" onClick={() => setConfirming(true)}>{t("Install")}</Button>
          )}
          {phase === "working" && (
            <p role="status">
              {t("Installing…")}{" "}
              <Button onClick={() => abort?.abort()}>{t("Cancel")}</Button>
            </p>
          )}
          {installed && (
            <p role="status">
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
