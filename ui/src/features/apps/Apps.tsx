import { useEffect, useMemo, useState } from "react";
import Link from "../../ui/Link";
import Button from "../../ui/Button";
import { appExplorer, type HomeInstallation, type FavoritesPayload } from "../workspace-apps/workspaceApps";
import type { Viewer } from "../projects/work";
import { AppGlyph } from "../explorer/shared";
import "../explorer/explorer.css";

/**
 * The Apps home (CAD-1129, `/apps`): the OS-style landing — a Needs-
 * attention rail, the operator's/member's favorites row, the installed
 * grid and (operator) Recently removed. "Open" opens the app's own
 * page (`/app-installations/<id>`); "Manage" opens its operator tab.
 *
 * The legacy project-scoped list and the per-project catalog live
 * behind `/apps/<project>/<name>` (unchanged); this screen is the
 * workspace-level home the ticket describes.
 */
export default function Apps({ viewer }: { project: string; viewer: Viewer }) {
  const [home, setHome] = useState<HomeInstallation[] | null>(null);
  const [favs, setFavs] = useState<FavoritesPayload | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const [q, setQ] = useState("");
  const [sort, setSort] = useState<"recent" | "name" | "attention">("recent");
  const [busy, setBusy] = useState<string | null>(null);

  const isOp = viewer.operator && !viewer.readOnly;

  useEffect(() => {
    const controller = new AbortController();
    setError(null);
    void appExplorer.home(controller.signal)
      .then((r) => { if (!controller.signal.aborted) setHome(r.installations); })
      .catch((e: unknown) => { if (!controller.signal.aborted) setError(e instanceof Error ? e.message : "Could not load your apps"); });
    void appExplorer.favorites(controller.signal)
      .then((f) => { if (!controller.signal.aborted) setFavs(f); })
      .catch(() => { if (!controller.signal.aborted) setFavs(null); });
    return () => controller.abort();
  }, [revision]);

  const byId = useMemo(() => {
    const map = new Map<string, HomeInstallation>();
    (home ?? []).forEach((h) => map.set(h.install_id, h));
    return map;
  }, [home]);

  const live = useMemo(() => (home ?? []).filter((h) => h.attention.state !== "removed"), [home]);
  const removedList = useMemo(() => (home ?? []).filter((h) => h.attention.state === "removed"), [home]);

  const attention = useMemo(() => live.filter((h) => {
    const s = h.attention.state;
    return s === "setup" || s === "attention" || (s === "update" && isOp) || s === "off";
  }), [live, isOp]);

  const favorites = useMemo(() => {
    const ids = favs?.favorites ?? [];
    return ids.map((id) => byId.get(id)).filter((h): h is HomeInstallation => !!h);
  }, [favs, byId]);

  const items = useMemo(() => {
    const needle = q.trim().toLowerCase();
    let list = live.filter((h) => !needle || [h.name, h.title, h.tagline].filter(Boolean).join(" ").toLowerCase().includes(needle));
    if (sort === "name") list = [...list].sort((a, b) => a.title.localeCompare(b.title));
    if (sort === "attention") {
      const rank: Record<string, number> = { attention: 0, setup: 1, update: 2, off: 3, ok: 4 };
      list = [...list].sort((a, b) => (rank[a.attention.state] ?? 4) - (rank[b.attention.state] ?? 4));
    }
    if (sort === "recent" && favs?.recent) {
      const order = new Map(favs.recent.map((id, i) => [id, i]));
      list = [...list].sort((a, b) => (order.get(a.install_id) ?? 999) - (order.get(b.install_id) ?? 999));
    }
    return list;
  }, [live, q, sort, favs]);

  const pin = (id: string) => {
    const cur = favs?.favorites ?? [];
    const next = cur.includes(id) ? cur.filter((f) => f !== id) : [...cur, id];
    setFavs(favs ? { ...favs, favorites: next } : { favorites: next, workspace_default: [], recent: [] });
    void appExplorer.putFavorites(next).catch(() => setRevision((r) => r + 1));
  };
  const opened = (id: string) => { void appExplorer.opened(id).catch(() => {}); };
  const restore = (id: string) => {
    setBusy(id);
    void appExplorer.restore(id)
      .then(() => setRevision((r) => r + 1))
      .catch((e: unknown) => setError(e instanceof Error ? e.message : "Restore was refused"))
      .finally(() => setBusy(null));
  };

  const attentionRow = (h: HomeInstallation) => {
    const s = h.attention.state;
    const label = s === "off" ? "Access off" : s === "setup" ? "Finish setup" : s === "update" ? "Update ready" : "Needs attention";
    return (
      <div key={h.install_id} className="attn-row">
        <AppGlyph name={h.name} icon={h.icon} size="sm" />
        <span className="txt"><b>{h.title}</b> · {h.attention.message ?? label}</span>
        {isOp ? (
          <Button className="btn-sm" href={s === "off" || s === "update" ? `/apps/manage/${h.install_id}` : `/app-installations/${h.install_id}`}>
            {s === "off" ? "Manage access" : s === "update" ? "Review update" : "Finish setup"}
          </Button>
        ) : (
          <span className="hint">An admin needs to handle this</span>
        )}
      </div>
    );
  };

  return (
    <main className="apps-home px-4 lg:px-8 pt-4 pb-9 min-w-0" aria-label="apps">
      <div className="ohead flex flex-wrap items-center gap-3 mb-2">
        <h1 className="text-section font-semibold text-ink-100">Apps</h1>
        <span className="grow" />
        <Button className="btn-primary" href="/apps/explore">Explore apps</Button>
      </div>

      {error ? (
        <div className="card px-4 py-5 mb-4" role="alert">
          <h3 className="font-medium text-ink-100 mb-1">Couldn't load your apps</h3>
          <p className="text-label text-ink-400 mb-3">{error}</p>
          <Button onClick={() => setRevision((r) => r + 1)}>Try again</Button>
        </div>
      ) : home === null ? (
        <p className="text-label text-ink-400" role="status">Loading your apps…</p>
      ) : (
        <>
          {live.length === 0 && (
            <div className="card px-4 py-8 text-center my-4">
              <h3 className="text-cardtitle font-medium text-ink-100 mb-1">No apps yet</h3>
              <p className="text-secondary text-ink-400 mb-4">
                {isOp
                  ? "Apps add new skills to your workspace — a customer list, social posts and more. Browse what's available and install one in a tap."
                  : "Your admin hasn't installed any apps yet. Browse what's available and ask for one."}
              </p>
              <Button className="btn-primary btn-lg" href="/apps/explore">Explore apps</Button>
            </div>
          )}

          {attention.length > 0 && (
            <section className="attn" aria-label="Needs attention">
              <div className="attn-head"><span aria-hidden>⚠</span>{attention.length} {attention.length === 1 ? "app needs" : "apps need"} attention</div>
              {attention.map(attentionRow)}
            </section>
          )}

          <section className="sec" aria-label="Favorites">
            <div className="sechead">
              <h2>Favorites</h2>
            </div>
            {favorites.length > 0 ? (
              <div className="favs">
                {favorites.map((h) => (
                  <Link key={h.install_id} href={`/app-installations/${h.install_id}`} className="card fav"
                    onClick={() => opened(h.install_id)} aria-label={`Open ${h.title}`}>
                    <AppGlyph name={h.name} icon={h.icon} />
                    <span className="nm">{h.title}</span>
                    <span className="ln">{h.tagline}</span>
                    <button className="star pinned absolute top-2 right-2" aria-label={`Unpin ${h.title}`}
                      onClick={(e) => { e.preventDefault(); e.stopPropagation(); pin(h.install_id); }}>★</button>
                  </Link>
                ))}
              </div>
            ) : (
              <div className="favempty">
                <span aria-hidden>★</span>
                <span>Pin the apps you use every day. Tap the star on any app below and it appears here and in the sidebar.</span>
              </div>
            )}
          </section>

          {live.length > 0 && (
            <section className="sec" aria-label="Installed">
              <div className="sechead">
                <h2>Installed</h2>
                <span className="chip">{live.length}</span>
                <span className="grow" />
                <input className="field" style={{ width: "min(220px,100%)" }} placeholder="Search your apps" value={q} onChange={(e) => setQ(e.target.value)} aria-label="Search installed apps" />
                <select className="field" style={{ width: "auto" }} value={sort} onChange={(e) => setSort(e.target.value as typeof sort)} aria-label="Sort">
                  <option value="recent">Recently used</option>
                  <option value="name">Name</option>
                  <option value="attention">Needs attention first</option>
                </select>
              </div>
              {items.length === 0 ? (
                <div className="card px-4 py-5 text-secondary text-ink-400">
                  No installed app matches “{q}” — try another word, or look in <Link href="/apps/explore">Explore</Link>.
                </div>
              ) : (
                <div className="igrid">
                  {items.map((h) => {
                    const pinned = favs?.favorites.includes(h.install_id) ?? false;
                    return (
                      <Link key={h.install_id} className="card itile" href={`/app-installations/${h.install_id}`}
                        onClick={() => opened(h.install_id)} aria-label={`Open ${h.title}`}>
                        <AppGlyph name={h.name} icon={h.icon} size="sm" />
                        <div className="min-w-0 flex-1">
                          <div className="nm">{h.title}</div>
                          <div className="ln">{h.tagline}</div>
                          {h.project && <div className="ln text-ink-500">Project: {h.project}</div>}
                          {h.attention.state !== "ok" && (
                            <div className="mt-1"><span className={`chip ${h.attention.state === "update" ? "info" : h.attention.state === "off" ? "" : "warn"}`}>
                              {h.attention.state === "update" ? "Update" : h.attention.state === "off" ? "Access off" : h.attention.state === "setup" ? "Finish setup" : "Needs attention"}
                            </span></div>
                          )}
                        </div>
                        <div className="acts">
                          <button className={`star ${pinned ? "pinned" : ""}`} aria-pressed={pinned}
                            aria-label={`${pinned ? "Unpin" : "Pin"} ${h.title}`}
                            onClick={(e) => { e.preventDefault(); e.stopPropagation(); pin(h.install_id); }}>{pinned ? "★" : "☆"}</button>
                          {isOp && <Link className="star" href={`/apps/manage/${h.install_id}`} aria-label={`Manage ${h.title}`} onClick={(e) => e.stopPropagation()}>⚙</Link>}
                        </div>
                      </Link>
                    );
                  })}
                </div>
              )}
              <Link href="/apps/explore" className="explore-card mt-3">
                <span aria-hidden>⌖</span>
                <span className="flex-1"><b>Find more apps</b><small className="block">Bookings, reviews, invoices and more, made for small businesses.</small></span>
                <span aria-hidden>→</span>
              </Link>
            </section>
          )}

          {isOp && removedList.length > 0 && (
            <section className="sec" aria-label="Recently removed">
              <div className="sechead"><h2>Recently removed</h2><span className="chip">{removedList.length}</span></div>
              <p className="hint mb-2">Removed apps restore for 30 days, then they're deleted for good.</p>
              <div className="card p-0 divide-y divide-ink-800">
                {removedList.map((h) => (
                  <div key={h.install_id} className="rrow flex items-center gap-3 p-3">
                    <AppGlyph name={h.name} icon={h.icon} size="sm" />
                    <div className="grow"><b>{h.title}</b><small className="block text-ink-500">Removed</small></div>
                    <Button className="btn-sm" disabled={busy === h.install_id} onClick={() => restore(h.install_id)}>
                      {busy === h.install_id ? "Restoring…" : "Restore"}
                    </Button>
                  </div>
                ))}
              </div>
            </section>
          )}
        </>
      )}
    </main>
  );
}
