/** Valid ISO text or daemon epoch seconds as milliseconds; zero means unavailable. */
export function activityTimeMs(value: string | number | null | undefined): number {
  if (typeof value === "string") {
    const parts = value.match(
      /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/,
    );
    if (!parts) return 0;
    const [year, month, day, hour, minute, second] = parts.slice(1).map(Number);
    const leap = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
    const days = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if (
      month < 1 || month > 12 || day < 1 || day > days[month - 1] ||
      hour > 23 || minute > 59 || second > 59
    ) return 0;
  }
  const stamp = typeof value === "number" ? value * 1000 : Date.parse(value ?? "");
  return Number.isFinite(stamp) && stamp > 0 && stamp <= 8.64e15 ? stamp : 0;
}

/// ISO text or daemon epoch seconds → `17 Sep 18:27`; other text passes through.
export function fmtTime(value: string | number): string {
  const stamp = typeof value === "number" ? activityTimeMs(value) : 0;
  const iso = typeof value === "number"
    ? stamp ? new Date(stamp).toISOString() : "Time unavailable"
    : value;
  const m = iso.match(
    /^(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2})/,
  );
  if (!m) return iso;
  const months = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun",
    "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
  ];
  return `${+m[3]} ${months[+m[2] - 1]} ${m[4]}:${m[5]}`;
}

export function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

/** `x.y.z+<7-hex>` for a release, the short SHA without a version. */
export function releaseLabel(
  version: string | null | undefined,
  sha: string | null | undefined,
): string {
  if (version)
    return version.replace(
      /\+([a-f0-9]{40})$/,
      (_, build: string) => `+${build.slice(0, 7)}`,
    );
  return sha ? sha.slice(0, 7) : "Not installed";
}

/**
 * The running build as the account menu shows it (CAD-1034): the serving
 * binary's release label from `Meta`, or null when meta carries no version.
 */
export function runningRelease(
  meta: { version?: string | null; build_commit?: string | null } | null | undefined,
): string | null {
  if (!meta?.version) return null;
  const commit = meta.build_commit ?? "";
  return releaseLabel(!meta.version.includes("+") && /^[a-f0-9]{7,40}$/.test(commit) ? `${meta.version}+${commit}` : meta.version, commit);
}
