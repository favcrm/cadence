import { useMemo, useState } from "react";
import Link from "../../ui/Link";
import Button from "../../ui/Button";
import { appExplorer, notifyAppsChanged, type CatalogCard } from "../workspace-apps/workspaceApps";
import type { Viewer } from "../projects/work";
import { AppGlyph, TrustChip, InstallStateChip, useEscape } from "./shared";
import { appErrorCopy, appLoadErrorCopy } from "./appErrors";
import InstallCheckPanel from "./InstallCheckPanel";
import { Loading, Notice } from "../app-shell/shared/States";
import { navigate } from "../../lib/useLocation";
import { useBackoffLoad } from "../workspace-apps/useBackoffLoad";
import "./explorer.css";

/** A card opens its detail page through the router. */
const openDetail = (id: string) => navigate(`/apps/catalog/${encodeURIComponent(id)}`);

/**
 * The Apps Explorer (CAD-1129, `/apps/explore`): the catalog the host
 * ships — every built-in as a card saying what it can see before the
 * install. An operator's card carries an Install button and a request
 * count; a member's shows Request and "Installing is up to your
 * admin". A Git URL is the operator's "More" — checked live, it opens
 * as an "unverified app" card, never a catalog row.
 */
export default function Explorer({ viewer }: { viewer: Viewer }) {
  const [revision, setRevision] = useState(0);
  const [q, setQ] = useState("");
  const [cat, setCat] = useState("All");
  const [gitOpen, setGitOpen] = useState(false);
  const [busy, setBusy] = useState<Record<string, boolean>>({});
  const [notice, setNotice] = useState<string | null>(null);
  const [reviewing, setReviewing] = useState<{ id: string; title: string } | null>(null);

  const isOp = viewer.operator === true && !viewer.readOnly;

  // CAD-1189: the catalog and the request list load with the shared busy
  // backoff, keep their last good answer on a failed refresh and never
  // read an error as "no apps".
  const catalogLoad = useBackoffLoad((signal) => appExplorer.catalog(signal).then((r) => r.catalog), revision, viewer.operator !== null);
  const requestsLoad = useBackoffLoad((signal) => appExplorer.requests(signal).then((r) => r.requests), revision, isOp);
  const catalog = catalogLoad.data;
  const requests = requestsLoad.data;
  const error = catalogLoad.error === null ? null : appLoadErrorCopy(catalogLoad.error, "The catalog didn't load.");

  const cats = useMemo(() => {
    const set = new Set<string>();
    (catalog ?? []).forEach((c) => { if (c.listing?.category) set.add(c.listing.category); });
    return ["All", ...Array.from(set).sort()];
  }, [catalog]);

  const items = useMemo(() => {
    const needle = q.trim().toLowerCase();
    return (catalog ?? []).filter((c) => {
      if (cat !== "All" && c.listing?.category !== cat) return false;
      if (!needle) return true;
      return [c.name, c.title, c.listing?.tagline, c.listing?.category, ...(c.can ?? c.listing?.can ?? [])]
        .filter(Boolean).join(" ").toLowerCase().includes(needle);
    });
  }, [catalog, q, cat]);

  const featured = useMemo(() => catalog?.filter((c) => c.featured) ?? [], [catalog]);

  // Every install starts with a check (CAD-1194): the panel shows what
  // the built-in would install and installs the checked digest.
  const install = (id: string) => {
    setNotice(null);
    setReviewing({ id, title: catalog?.find((c) => c.id === id)?.title ?? id });
  };
  const requestInstall = (id: string) => {
    setBusy((b) => ({ ...b, [id]: true }));
    void appExplorer.requestInstall(id)
      .then(() => { setNotice("Asked — your admin will see the request."); setRevision((r) => r + 1); })
      .catch((e: unknown) => setNotice(appErrorCopy(e, "Could not send the request. Try again in a moment.")))
      .finally(() => setBusy((b) => ({ ...b, [id]: false })));
  };
  // A removed app comes back with Restore, never a second Install.
  const restore = (card: CatalogCard) => {
    if (!card.install_id) return;
    const id = card.id;
    setBusy((b) => ({ ...b, [id]: true }));
    void appExplorer.restore(card.install_id)
      .then(() => { setNotice(`Restored — ${card.title} is live again.`); setRevision((r) => r + 1); notifyAppsChanged(); })
      .catch((e: unknown) => setNotice(appErrorCopy(e, "Restore didn't finish. Try again in a moment.")))
      .finally(() => setBusy((b) => ({ ...b, [id]: false })));
  };

  if (viewer.operator === null) {
    return (
      <main className="apps-explore px-4 lg:px-8 pt-4 pb-9 min-w-0" aria-label="explore apps">
        <h1 className="text-section font-semibold text-ink-100 mb-4">Explore apps</h1>
        {viewer.access === "unavailable" ? (
          <Notice state="access-unavailable" onRetry={viewer.onRetryAccess} retryLabel="Retry access check">
            Access could not be confirmed. Retry the access check before acting.
          </Notice>
        ) : <Loading>Checking whether this session can act…</Loading>}
      </main>
    );
  }

  return (
    <main className="apps-explore px-4 lg:px-8 pt-4 pb-9 min-w-0" aria-label="explore apps">
      <div className="ohead flex flex-wrap items-center gap-3 mb-2">
        <h1 className="text-section font-semibold text-ink-100">Explore apps</h1>
        <span className="grow" />
        <div className="popwrap relative">
          <Button className="btn-sm" onClick={() => setGitOpen((v) => !v)} aria-expanded={gitOpen}>
            More ▾
          </Button>
          {gitOpen && (
            <div className="pmenu" role="menu">
              {isOp ? (
                <button className="w-full text-left" onClick={() => setGitOpen(false)} data-git>
                  Add from Git URL…<small className="block text-ink-500">For custom apps your team built</small>
                </button>
              ) : (
                <button className="w-full text-left opacity-50" disabled>
                  Add from Git URL<small className="block text-ink-500">Admins only</small>
                </button>
              )}
              <Link href="/apps" className="block">Your installed apps</Link>
            </div>
          )}
        </div>
      </div>
      <p className="text-secondary text-ink-400 mb-4">
        Apps add new skills to your workspace. Each one says what it can see before you install.
      </p>

      {isOp && gitOpen && (
        <GitAdd
          onDone={() => { setGitOpen(false); setNotice("Installed — it's in your apps now."); setRevision((r) => r + 1); notifyAppsChanged(); }}
          onClose={() => setGitOpen(false)}
        />
      )}

      {isOp && reviewing && (
        <InstallCheckPanel
          key={reviewing.id}
          source={`builtin:${reviewing.id}`}
          label={reviewing.title}
          install={(digest) => appExplorer.installEntry(reviewing.id, digest)}
          onInstalled={() => { setReviewing(null); setNotice("Installed — it's in your apps now."); setRevision((r) => r + 1); notifyAppsChanged(); }}
          onClose={() => setReviewing(null)}
        />
      )}

      {notice && <div className="alert info mb-4" role="status">{notice}</div>}

      {isOp && requests && requests.length > 0 && (
        <div className="alert info mb-4" role="status">
          {requests.length} {requests.length === 1 ? "teammate asked" : "teammates asked"} for an app — see the request count on its card.
        </div>
      )}

      {error !== null && (
        <div className="err card px-4 py-3 mb-4 text-label text-ink-400" role="alert">
          <b className="text-ink-100">Couldn't load the app catalog.</b> {error} <Button onClick={() => setRevision((r) => r + 1)}>Try again</Button>
        </div>
      )}
      {catalogLoad.retrying && <p className="text-label text-ink-400 mb-2" role="status">The workspace is busy. Retrying…</p>}
      {catalog === null ? (
        error === null && !catalogLoad.retrying && <p className="text-label text-ink-400" role="status">Loading the catalog…</p>
      ) : (
        <>
          <div className="flex flex-wrap items-center gap-3 mb-5">
            <input
              className="field flex-1 min-w-[220px]"
              placeholder="Search apps, e.g. “email”, “bookings”"
              value={q}
              onChange={(e) => setQ(e.target.value)}
              aria-label="Search apps"
            />
            <div className="cats flex flex-wrap gap-1.5" role="group" aria-label="Categories">
              {cats.map((c) => (
                <button key={c} className={`cat ${cat === c ? "on" : ""}`} onClick={() => setCat(c)} aria-pressed={cat === c}>{c}</button>
              ))}
            </div>
          </div>

          {!isOp && (
            <div className="alert info mb-4">
              You can browse and open installed apps. Installing is up to your admin — tap <b>Ask an admin</b> on any app.
            </div>
          )}

          {!q && cat === "All" && featured.length > 0 && (
            <section className="sec mb-6" aria-label="Featured">
              <h2 className="text-cardtitle font-medium text-ink-100 mb-2">Made for your business</h2>
              <div className="feat grid gap-3 md:grid-cols-2">
                {featured.map((c) => (
                  <div key={c.id} className="card fcard p-4 cursor-pointer" role="button" tabIndex={0}
                    onClick={() => openDetail(c.id)}
                    onKeyDown={(e) => e.key === "Enter" && openDetail(c.id)}>
                    <span className="why text-micro text-ink-500">Popular with shops like yours</span>
                    <div className="flex items-center gap-3 mt-1">
                      <AppGlyph name={c.name} icon={c.listing?.icon} size="sm" />
                      <h3 className="text-cardtitle font-medium text-ink-100">{c.title}</h3>
                    </div>
                    <p className="text-label text-ink-400 mt-2">{c.listing?.tagline}</p>
                    <div className="flex items-center justify-end mt-3">
                      <CardAction card={c} isOp={isOp} busy={busy[c.id]} onInstall={() => install(c.id)} onRequest={() => requestInstall(c.id)} onRestore={() => restore(c)} />
                    </div>
                  </div>
                ))}
              </div>
            </section>
          )}

          <section className="sec" aria-label="All apps">
            <div className="sechead flex items-center gap-2 mb-3">
              <h2 className="text-cardtitle font-medium text-ink-100">
                {q ? `Results for “${q}”` : cat === "All" ? "All apps" : cat}
              </h2>
              <span className="chip">{items.length}</span>
            </div>
            {items.length === 0 ? (
              <div className="card px-4 py-5 text-secondary text-ink-400">
                <h3 className="font-medium text-ink-100 mb-1">No apps match “{q}”</h3>
                <p>Try a simpler word like “email” or “posts”.</p>
                {isOp && <Button className="mt-3" onClick={() => setGitOpen(true)}>Add from Git URL</Button>}
              </div>
            ) : (
              <div className="cgrid grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
                {items.map((c) => (
                  <CatalogCardView key={c.id} card={c} isOp={isOp} busy={busy[c.id]} onInstall={() => install(c.id)} onRequest={() => requestInstall(c.id)} onRestore={() => restore(c)} />
                ))}
              </div>
            )}
          </section>
        </>
      )}
    </main>
  );
}

function CatalogCardView({ card, isOp, busy, onInstall, onRequest, onRestore }: {
  card: CatalogCard; isOp: boolean; busy?: boolean; onInstall: () => void; onRequest: () => void; onRestore: () => void;
}) {
  return (
    <div className="card ccard p-4 flex flex-col gap-2" role="button" tabIndex={0}
      onClick={() => openDetail(card.id)}
      onKeyDown={(e) => e.key === "Enter" && openDetail(card.id)}>
      <div className="flex items-start gap-3">
        <AppGlyph name={card.name} icon={card.listing?.icon} />
        <div className="min-w-0">
          <div className="nm text-cardtitle font-medium text-ink-100 truncate">{card.title}</div>
          <div className="pub text-micro text-ink-500">
            <TrustChip trust={card.trust} /> · {card.listing?.category ?? "App"}
          </div>
        </div>
      </div>
      <p className="text-label text-ink-400 flex-1">{card.listing?.tagline}</p>
      <div className="flex items-center justify-between mt-1">
        <InstallStateChip state={card.removed ? "removed" : card.state} />
        <CardAction card={card} isOp={isOp} busy={busy} onInstall={onInstall} onRequest={onRequest} onRestore={onRestore} />
      </div>
      {isOp && (card.request_count ?? 0) > 0 && (
        <div className="text-micro text-ink-500">{card.request_count} teammate{card.request_count === 1 ? "" : "s"} asked for this</div>
      )}
    </div>
  );
}

/** The one action a card's corner offers: Install (operator), Ask an
 * admin (member, then "Asked"), or Open when installed. */
function CardAction({ card, isOp, busy, onInstall, onRequest, onRestore }: {
  card: CatalogCard; isOp: boolean; busy?: boolean; onInstall: () => void; onRequest: () => void; onRestore: () => void;
}) {
  if (busy) return <Button className="btn-sm btn-primary" disabled>Working…</Button>;
  if (card.removed) {
    if (!isOp) return <span className="text-micro text-ink-500">Removed</span>;
    return card.restorable
      ? <Button className="btn-sm" onClick={(e) => { e.stopPropagation(); onRestore(); }}>Restore</Button>
      : <span className="text-micro text-ink-500">Restore window closed</span>;
  }
  if (card.state === "installed" || card.state === "off") {
    return <Link className="btn btn-sm" href={`/app-installations/${card.install_id}`} onClick={(e) => e.stopPropagation()}>Open</Link>;
  }
  if (card.state === "requested" || card.requested_by_me) {
    return <Button className="btn-sm" disabled>Asked</Button>;
  }
  if (!isOp) {
    return <Button className="btn-sm" onClick={(e) => { e.stopPropagation(); onRequest(); }}>Ask an admin</Button>;
  }
  return <Button className="btn-sm btn-primary" onClick={(e) => { e.stopPropagation(); onInstall(); }}>Install</Button>;
}

/** The operator's "Add from Git URL" — a live check against the repo
 * (HTTPS, public host), then the install review: the daemon's
 * install-check shows exactly what would be installed and the install
 * sends that digest. Never a catalog row: it ships nothing to the
 * built-in list. */
function GitAdd({ onDone, onClose }: { onDone: () => void; onClose: () => void }) {
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
    <div className="card p-4 mb-4" role="group" aria-label="Add from Git URL">
      <h3 className="font-medium text-ink-100 mb-2">Add from Git URL</h3>
      <input className="field w-full" placeholder="https://github.com/your-team/app" value={url}
        onChange={(e) => { setUrl(e.target.value); setFound(null); }} aria-label="Git URL" />
      <div className="flex gap-2 mt-3">
        <Button className="btn-sm btn-primary" disabled={checking || !url.startsWith("https://")} onClick={check}>
          {checking ? "Checking…" : "Check it"}
        </Button>
        <Button className="btn-sm" onClick={onClose}>Close</Button>
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
          <div className="alert warn mb-2">Added from a Git URL by your team. Cadence checked its file list but hasn't reviewed what it does.</div>
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
