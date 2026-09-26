/* Apps.tsx — minimal /apps landing. The real Apps page (installed apps,
 * per-project entries, /apps/<project>/<app>) is CAD-557 by swe-apps; this
 * placeholder exists only so the Apps nav entry lands somewhere sensible
 * until #307 rebases onto it. Keep it this thin on purpose.
 */
import Link from "../../ui/Link";

export default function Apps({ socialHref }: { socialHref: string }) {
  return (
    <main className="px-4 lg:px-8 pt-6 pb-9">
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1.5 mb-4">
        <h1 className="text-section font-semibold text-ink-100 leading-tight">Apps</h1>
        <span className="kicker">installed</span>
      </div>
      <div className="flex flex-col gap-2 max-w-xl">
        <Link href={socialHref} className="card tcard p-3 flex items-center gap-2.5 group">
          <span className="sc-avatar" style={{ background: "#e05d44" }}>蔵</span>
          <div className="min-w-0">
            <div className="text-label font-medium text-ink-100 group-hover:text-accent">
              Social Content
            </div>
            <div className="text-micro text-ink-500">drafts → review → schedule, per client</div>
          </div>
          <span className="ml-auto chip bg-ok/15 text-ok">installed</span>
        </Link>
        <div className="card p-3 flex items-center gap-2.5 opacity-70">
          <span className="sc-avatar" style={{ background: "var(--color-ink-700)" }}>B</span>
          <div className="min-w-0">
            <div className="text-label font-medium text-ink-100">Blog Post</div>
            <div className="text-micro text-ink-500">invoked from Social Content — “Write a blog post”</div>
          </div>
          <span className="ml-auto chip bg-ink-800 text-ink-400">installed</span>
        </div>
        <div className="card p-3 border-dashed opacity-50">
          <div className="text-label text-ink-300">Explore</div>
          <div className="text-micro text-ink-500">the app directory isn't open yet — CAD-557</div>
        </div>
      </div>
    </main>
  );
}
