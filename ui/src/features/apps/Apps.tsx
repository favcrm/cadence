/* Apps.tsx — the Installed list at /apps. Each card is one installed app;
 * the social-content card links into its Home. No marketplace yet — Explore
 * stays a disabled hint so nobody looks for a store that doesn't exist.
 */
import Link from "../../ui/Link";

export default function Apps({ socialHref }: { socialHref: string }) {
  return (
    <main className="px-4 lg:px-8 pt-6 pb-9">
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1.5 mb-4">
        <h1 className="text-section font-semibold text-ink-100 leading-tight">Apps</h1>
        <span className="kicker">installed on this board</span>
      </div>

      <div className="grid gap-3" style={{ gridTemplateColumns: "repeat(auto-fill, minmax(280px, 1fr))" }}>
        <Link href={socialHref} className="card tcard p-4 block group">
          <div className="flex items-center gap-2.5 mb-2">
            <span className="sc-avatar" style={{ background: "#e05d44" }}>蔵</span>
            <div className="min-w-0">
              <div className="text-cardtitle font-semibold text-ink-100 group-hover:text-accent">
                Social Content
              </div>
              <div className="text-micro text-ink-500">social-localize · scan-sources · revise</div>
            </div>
            <span className="ml-auto chip bg-ok/15 text-ok">installed</span>
          </div>
          <p className="text-label text-ink-400 leading-relaxed">
            Draft localised posts from each client's feed, run them through review, and ship the
            week's schedule through one approval digest. Two clients on this board.
          </p>
          <div className="mt-2.5 flex items-center gap-2 text-label">
            <span className="text-ink-500">open →</span>
            <span className="text-ink-500">Home · Library · Runs · Needs you</span>
          </div>
        </Link>

        <div className="card p-4 opacity-75">
          <div className="flex items-center gap-2.5 mb-2">
            <span className="sc-avatar" style={{ background: "var(--color-ink-700)" }}>B</span>
            <div className="min-w-0">
              <div className="text-cardtitle font-semibold text-ink-100">Blog Post</div>
              <div className="text-micro text-ink-500">blog-post</div>
            </div>
            <span className="ml-auto chip bg-ink-800 text-ink-400">installed</span>
          </div>
          <p className="text-label text-ink-400 leading-relaxed">
            Long-form posts for a client's site — brief → outline → draft → images → review. Starts
            outside the drafting standing approval, so a plan card opens first. It's invoked from
            inside Social Content ("Write a blog post").
          </p>
        </div>

        <div className="card p-4 opacity-50 border-dashed" aria-disabled>
          <div className="text-cardtitle font-semibold text-ink-300 mb-1">Explore</div>
          <p className="text-label text-ink-500">The app directory isn't open yet — coming later.</p>
        </div>
      </div>
    </main>
  );
}
