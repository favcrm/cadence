import { useEffect, useState } from "react";
import Link from "../../ui/Link";
import { useTranslate } from "../../lib/locale";
import type { Viewer } from "../projects/work";
import { AppGlyph, TrustChip } from "./shared";
import { ErrorNotice, Loading, Notice } from "../app-shell/shared/States";
import { useHref } from "../../lib/useLocation";
import { fetchPublicApp, platformLoginHref, unavailableCopy, type PublicApp } from "./publicCatalogue";
import "./explorer.css";

type Phase = "loading" | "ok" | "not_found" | "malformed" | "unavailable";

/**
 * One public catalogue app (`/apps/catalog/<id>`). The same JSON loads
 * whether or not the viewer is signed in. Install is not offered.
 */
export default function CatalogDetail({ id, viewer }: { id: string; viewer: Viewer }) {
  const t = useTranslate();
  const href = useHref();
  const [reload, setReload] = useState(0);
  const [phase, setPhase] = useState<Phase>("loading");
  const [app, setApp] = useState<PublicApp | null>(null);
  const confirmed = viewer.operator === true;

  useEffect(() => {
    const controller = new AbortController();
    setPhase("loading");
    setApp(null);
    void fetchPublicApp(id, controller.signal)
      .then((result) => {
        if (controller.signal.aborted) return;
        if (result.status === "ok") {
          setApp(result.app);
          setPhase("ok");
        } else {
          setApp(null);
          setPhase(result.status);
        }
      })
      .catch((error: unknown) => {
        if (controller.signal.aborted || (error instanceof DOMException && error.name === "AbortError")) return;
        setApp(null);
        setPhase("unavailable");
      });
    return () => controller.abort();
  }, [id, reload]);

  if (phase === "loading") {
    return (
      <main className="apps-detail px-4 lg:px-8 pt-4 pb-9" aria-label={t("app catalog detail")}>
        <Loading>{t("Loading…")}</Loading>
      </main>
    );
  }
  if (phase === "not_found") {
    return (
      <main className="apps-detail px-4 lg:px-8 pt-4 pb-9" aria-label={t("app catalog detail")}>
        <div className="card px-4 py-5" role="status">
          <h2 className="font-medium text-ink-100 mb-1">{t("App not found")}</h2>
          <p className="text-label text-ink-400 mb-3">{t("This app isn't in the public catalogue.")}</p>
          <Link href="/apps/explore" className="btn">{t("Back to Explore")}</Link>
        </div>
      </main>
    );
  }
  if (phase === "malformed" || phase === "unavailable" || !app) {
    return (
      <main className="apps-detail px-4 lg:px-8 pt-4 pb-9">
        <ErrorNotice onRetry={() => setReload((n) => n + 1)} retryLabel={t("Try again")}>
          {phase === "malformed"
            ? t("The catalogue response couldn't be read.")
            : t("The public catalogue is unavailable.")}
        </ErrorNotice>
      </main>
    );
  }

  const why = unavailableCopy(app);
  const login = platformLoginHref(href);
  const shots = app.listing.screenshots ?? [];
  const notes = app.listing.accessNotes ?? [];
  const setup = app.listing.setup ?? [];
  const data = app.listing.data;

  return (
    <main className="apps-detail px-4 lg:px-8 pt-4 pb-9 min-w-0" aria-label={app.title}>
      <Link href="/apps/explore" className="text-label text-ink-400 hover:text-ink-100">{t("← Explore")}</Link>

      {viewer.operator === null && viewer.access === "unavailable" && (
        <Notice className="mt-3" state="access-unavailable" onRetry={viewer.onRetryAccess} retryLabel="Retry access check">
          {t("Access could not be confirmed. Sign in before installing.")}
        </Notice>
      )}

      <div className="hero mt-3">
        <AppGlyph name={app.title} icon={app.listing.icon} size="lg" />
        <div className="min-w-0">
          <h1>{app.title}</h1>
          <p className="tag">{app.listing.tagline}</p>
          <div className="by">
            {app.listing.publisher.name === "Cadence" ? <TrustChip trust="cadence" /> : <span>{app.listing.publisher.name}</span>}
            <span>·</span>
            <span>{app.listing.category}</span>
          </div>
        </div>
      </div>

      <div className="dgrid">
        <div className="min-w-0">
          {shots.length > 0 && (
            <div className="shots" aria-label={t("Screenshots")}>
              {shots.map((shot, i) => (
                <div key={i} className="shot">
                  <div className="mui">{t("screenshot")}</div>
                  {shot.caption && <span className="cap">{shot.caption}</span>}
                </div>
              ))}
            </div>
          )}
          {app.summary && <p className="about">{app.summary}</p>}
          {app.listing.about && app.listing.about !== app.summary && <p className="about mt-3">{app.listing.about}</p>}

          {app.listing.can.length > 0 && (
            <div className="blk">
              <h2>{t("What it can do")}</h2>
              <ul className="can">
                {app.listing.can.map((line) => <li key={line}><span aria-hidden>✓</span><span>{line}</span></li>)}
              </ul>
            </div>
          )}

          {notes.length > 0 && (
            <div className="blk">
              <h2>{t("What it can access")}</h2>
              <div className="access">
                {notes.map((line) => (
                  <div key={line} className="acc"><span className="ai" aria-hidden>·</span><div><span>{line}</span></div></div>
                ))}
              </div>
            </div>
          )}

          {setup.length > 0 && (
            <div className="blk">
              <h2>{t("Setup")}</h2>
              <ul className="can">
                {setup.map((step) => (
                  <li key={step.label}>
                    <span aria-hidden>◦</span>
                    <span><b>{step.label}</b>{step.help && <span className="block text-ink-500">{step.help}</span>}</span>
                  </li>
                ))}
              </ul>
            </div>
          )}

          {data && ((data.stores && data.stores.length > 0) || data.personal) && (
            <div className="blk">
              <h2>{t("Your data")}</h2>
              <div className="access">
                {(data.stores ?? []).map((row) => <div key={row} className="acc"><span className="ai">●</span><div><b>{t("Keeps")}</b><span>{row}</span></div></div>)}
                {data.personal && <div className="alert warn mt-2">{t("Holds personal data.")}</div>}
              </div>
            </div>
          )}
        </div>

        <aside className="card side-card p-4 flex flex-col gap-3">
          {viewer.operator !== null && (
            <Link className="btn" href="/apps">{t("Your installed apps")}</Link>
          )}
          {!confirmed ? (
            <a
              className="btn btn-primary btn-lg"
              href={login}
              onClick={(event) => {
                if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
                event.preventDefault();
                window.location.assign(login);
              }}
            >
              {t("Sign in")}
            </a>
          )}
          <span className="text-micro text-ink-500">
            {why ?? t("Installing from this catalogue isn't available on this page.")}
          </span>
          <dl className="dl mt-3">
            <dt>{t("Made by")}</dt><dd>{app.listing.publisher.name}</dd>
            {app.version && <><dt>{t("Version")}</dt><dd>{app.version}</dd></>}
            <dt>{t("Category")}</dt><dd>{app.listing.category}</dd>
          </dl>
        </aside>
      </div>
    </main>
  );
}
