/// ISO text or daemon epoch seconds → `17 Sep 18:27`; other text passes through.
export function fmtTime(value: string | number): string {
  const iso = typeof value === "number"
    ? Number.isFinite(value) && value > 0 && value <= 8.64e12
      ? new Date(value * 1000).toISOString()
      : "Time unavailable"
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
