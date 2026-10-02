import Link from "./Link";

/**
 * The account menu's last line (CAD-1034): the running release, or the
 * bundle's build id when meta carries no version. Links to Settings ›
 * Update; "What's new" shows only while an update is pending.
 */
export default function VersionLine({
  release,
  build,
  href,
  updatePending,
}: {
  /** `releaseLabel` output, or null when no release version is known. */
  release: string | null;
  build: string;
  href: string;
  updatePending: boolean;
}) {
  return (
    <Link
      href={href}
      data-version-line
      className="num mt-1 flex justify-between gap-3 rounded border-t border-ink-700 px-2 pt-2 pb-1.5 text-micro text-ink-500 hover:text-accent"
    >
      <span>{release ? `v${release}` : `build ${build.slice(0, 7)}`}</span>
      {updatePending && <span className="text-ink-300">What's new</span>}
    </Link>
  );
}
