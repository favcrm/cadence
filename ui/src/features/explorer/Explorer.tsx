import { useEffect, useMemo, useState } from "react";
import Link from "../../ui/Link";
import Button from "../../ui/Button";
import { useTranslate } from "../../lib/locale";
import { appExplorer, notifyAppsChanged, type CatalogCard } from "../workspace-apps/workspaceApps";
import type { Viewer } from "../projects/work";
import { AppGlyph, TrustChip, useEscape } from "./shared";
import { appErrorCopy } from "./appErrors";
import { displayUiNotice, type UiNotice } from "../app-shell/campaign/useEmailDraft";
import InstallCheckPanel from "./InstallCheckPanel";
import { EmptyState, ErrorNotice, Loading, Notice } from "../app-shell/shared/States";
import { navigate, useHref } from "../../lib/useLocation";
import {
  fetchPublicApps,
  platformLoginHref,
  unavailableCopy,
  type PublicApp,
} from "./publicCatalogue";
import "./explorer.css";

function SignInLink({ pageHref }: { pageHref: string }) {
  const t = useTranslate();
  const href = platformLoginHref(pageHref);
  return (
    <a
      className="btn btn-sm"
      href={href}
      onClick={(event) => {
        if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
        event.preventDefault();
        window.location.assign(href);
      }}
    >
      {t("Sign in")}
    </a>
  );
}

function readFilters(href: string): { q: string; cat: string } {
  const search = href.includes("?") ? href.slice(href.indexOf("?") + 1) : "";
  const params = new URLSearchParams(search);
  return { q: params.get("q") ?? "", cat: params.get("cat") ?? "" };
}

function writeFilters(q: string, cat: string) {
  const params = new URLSearchParams();
  if (q) params.set("q", q);
  if (cat) params.set("cat", cat);
  const search = params.toString();
  navigate(`/apps/explore${search ? `?${search}` : ""}`, { replace: true });
}

function matches(app: PublicApp, q: string, cat: string): boolean {
  if (cat && app.listing.category.toLowerCase() !== cat.toLowerCase()) return false;
  const needle = q.trim().toLowerCase();
  if (!needle) return true;
  const hay = [app.title, app.summary, app.listing.tagline, app.listing.about, app.listing.publisher.name, app.listing.category, app.id]
    .join("\n")
    .toLowerCase();
  return hay.includes(needle);
}

/**
 * Public catalogue at `/apps/explore`. Listing data loads for every
 * visitor. Install from the catalogue is not offered. Operators keep
 * the checked Git URL install.
 */
export default function Explorer({ viewer }: { viewer: Viewer }) {
  const t = useTranslate();
  const href = useHref();
  const { q, cat } = readFilters(href);
  const [reload, setReload] = useState(0);
  const [menuOpen, setMenuOpen] = useState(false);
  const [gitOpen, setGitOpen] = useState(false);
  const [notice, setNotice] = useState<UiNotice | null>(null);
  const [phase, setPhase] = useState<"loading" | "ok" | "malformed" | "unavailable">("loading");
  const [apps, setApps] = useState<PublicApp[] | null>(null);

  const isOp = viewer.operator === true && !viewer.readOnly;
  const confirmed = viewer.operator === true;

  useEffect(() => {
    const controller = new AbortController();
    setPhase("loading");
    void fetchPublicApps(controller.signal)
      .then((result) => {
        if (controller.signal.aborted) return;
        if (result.status === "ok") {
          setApps(result.apps);
          setPhase("ok");
        } else {
          setApps(null);
          setPhase(result.status);
        }
      })
      .catch((error: unknown) => {
        if (controller.signal.aborted || (error instanceof DOMException && error.name === "AbortError")) return;
        setApps(null);
        setPhase("unavailable");
      });
    return () => controller.abort();
  }, [reload]);

  const categories = useMemo(() => {
    const names = new Set((apps ?? []).map((app) => app.listing.category));
    return [...names].sort((a, b) => a.localeCompare(b));
  }, [apps]);

  const visible = useMemo(
    () => (apps ?? []).filter((app) => matches(app, q, cat)),
    [apps, q, cat],
  );

  return (
    <main className="apps-explore px-4 lg:px-8 pt-4 pb-9 min-w-0" aria-label="explore apps">
      <div className="ohead flex flex-wrap items-center gap-3 mb-2">
        <h1 className="text-section font-semibold text-ink-100">{t("Explore apps")}</h1>
        <span className="grow" />
        {viewer.operator !== null && (
          <Link href="/apps" className="btn btn-sm">{t("Your installed apps")}</Link>
        )}
        {!confirmed && <SignInLink pageHref={href} />}
        <div className="popwrap relative">
          <Button className="btn-sm" onClick={() => setMenuOpen((v) => !v)} aria-expanded={menuOpen}>
            More ▾
          </Button>
          {menuOpen && (
            <div className="pmenu" role="menu">
              {isOp ? (
                <button
                  className="w-full text-left"
                  onClick={() => { setMenuOpen(false); setGitOpen(true); }}
                  data-git
                >
                  {t("Add from Git URL…")}<small className="block text-ink-500">{t("For custom apps your team built")}</small>
                </button>
              ) : (
                <button className="w-full text-left opacity-50" disabled>
                  Add from Git URL<small className="block text-ink-500">{confirmed ? "Admins only" : "Sign in as an admin"}</small>
                </button>
              )}
            </div>
          )}
        </div>
      </div>
      <p className="text-secondary text-ink-400 mb-4">
        {t("Apps add new skills to your workspace. Browse the public catalogue here.")}
      </p>

      {viewer.operator === null && viewer.access === "unavailable" && (
        <Notice className="mb-4" state="access-unavailable" onRetry={viewer.onRetryAccess} retryLabel="Retry access check">
          {t("Access could not be confirmed. The public catalogue is still listed. Sign in before installing.")}
        </Notice>
      )}

      {isOp && gitOpen && (
        <GitAdd
          onDone={() => { setGitOpen(false); setNotice("Installed — it's in your apps now."); notifyAppsChanged(); }}
          onClose={() => setGitOpen(false)}
        />
      )}

      {notice && <div className="alert info mb-4" role="status">{displayUiNotice(notice, t)}</div>}

      <div className="flex flex-wrap items-center gap-3 mb-4">
        <input
          className="field"
          value={q}
          placeholder={t("Search apps")}
          aria-label={t("Search apps")}
          onChange={(event) => writeFilters(event.target.value, cat)}
        />
        <div className="cats flex flex-wrap gap-2" role="group" aria-label={t("Category")}>
          <button type="button" className={cat ? "cat" : "cat on"} onClick={() => writeFilters(q, "")}>
            {t("All")}
          </button>
          {categories.map((name) => (
            <button
              key={name}
              type="button"
              className={cat.toLowerCase() === name.toLowerCase() ? "cat on" : "cat"}
              onClick={() => writeFilters(q, name)}
            >
              {name}
            </button>
          ))}
        </div>
      </div>

      {phase === "loading" && <Loading>{t("Loading the public catalogue…")}</Loading>}
      {phase === "malformed" && (
        <ErrorNotice onRetry={() => setReload((n) => n + 1)} retryLabel={t("Try again")}>
          {t("The catalogue response couldn't be read.")}
        </ErrorNotice>
      )}
      {phase === "unavailable" && (
        <ErrorNotice onRetry={() => setReload((n) => n + 1)} retryLabel={t("Try again")}>
          {t("The public catalogue is unavailable.")}
        </ErrorNotice>
      )}
      {phase === "ok" && apps && apps.length === 0 && (
        <EmptyState title={t("No public apps are listed.")} />
      )}
      {phase === "ok" && apps && apps.length > 0 && visible.length === 0 && (
        <EmptyState title={t("No apps match this search.")} />
      )}
      {phase === "ok" && visible.length > 0 && (
        <div className="cgrid grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
          {visible.map((app) => (
            <CatalogueCard key={app.id} app={app} />
          ))}
        </div>
      )}
    </main>
  );
}

function CatalogueCard({ app }: { app: PublicApp }) {
  const t = useTranslate();
  const why = unavailableCopy(app);
  return (
    <Link href={`/apps/catalog/${encodeURIComponent(app.id)}`} className="card ccard p-4 flex flex-col gap-2 no-underline">
      <div className="flex items-start gap-3">
        <AppGlyph name={app.title} icon={app.listing.icon} />
        <div className="min-w-0">
          <div className="nm text-cardtitle font-medium text-ink-100">{app.title}</div>
          <div className="pub text-micro text-ink-500">
            {app.listing.publisher.name === "Cadence" ? <TrustChip trust="cadence" /> : <span>{app.listing.publisher.name}</span>}
            {" · "}{app.listing.category}
            {app.version ? ` · ${app.version}` : ""}
          </div>
        </div>
      </div>
      <p className="text-label text-ink-400 flex-1">{app.listing.tagline || app.summary}</p>
      {why && <p className="text-micro text-ink-500">{t(why)}</p>}
    </Link>
  );
}

/** The operator's "Add from Git URL" — a live check, then install review. */
function GitAdd({ onDone, onClose }: { onDone: () => void; onClose: () => void }) {
  const t = useTranslate();
  const [url, setUrl] = useState("");
  const [checking, setChecking] = useState(false);
  const [found, setFound] = useState<CatalogCard | null>(null);
  const [err, setErr] = useState<string | null>(null);
  useEscape(true, onClose);
  const check = () => {
    setChecking(true); setErr(null); setFound(null);
    void appExplorer.gitCheck(url)
      .then((card) => setFound(card))
      .catch((e: unknown) => setErr(appErrorCopy(e, "That URL doesn't hold a Cadence app.")))
      .finally(() => setChecking(false));
  };
  return (
    <div className="card p-4 mb-4" role="group" aria-label={t("Add from Git URL")}>
      <h3 className="font-medium text-ink-100 mb-2">{t("Add from Git URL")}</h3>
      <input className="field w-full" placeholder="https://github.com/your-team/app" value={url}
        onChange={(e) => { setUrl(e.target.value); setFound(null); }} aria-label={t("Git URL")} />
      <div className="flex gap-2 mt-3">
        <Button className="btn-sm btn-primary" disabled={checking || !url.startsWith("https://")} onClick={check}>
          {checking ? t("Checking…") : t("Check it")}
        </Button>
        <Button className="btn-sm" onClick={onClose}>{t("Close")}</Button>
      </div>
      {err && <div className="alert fail mt-3" role="alert">{err}</div>}
      {found && (
        <div className="mt-3">
          <div className="flex items-center gap-3 mb-2">
            <AppGlyph name={found.name} icon={found.listing?.icon} />
            <div className="min-w-0">
              <div className="font-medium text-ink-100">{found.title}</div>
              <div className="text-micro text-ink-500"><TrustChip trust="unreviewed" /> · {found.version}</div>
            </div>
          </div>
          <div className="alert warn mb-2">{t("Added from a Git URL by your team. Cadence checked its file list but hasn't reviewed what it does.")}</div>
          <InstallCheckPanel
            key={`${url}@${found.commit}`}
            source={url}
            label={found.title}
            install={(digest) => appExplorer.installSource(url, digest)}
            onInstalled={onDone}
            onClose={() => setFound(null)}
          />
        </div>
      )}
    </div>
  );
}
