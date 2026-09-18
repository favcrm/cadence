// The cadence mark at UI sizes: the favicon geometry (one chevron + cursor)
// with the master icon's lit gradient. Sources: ui/public/icon.svg (master),
// ui/public/favicon.svg (flat), docs/brand/icon-mark.svg (transparent).
export function Logo({ size = 25 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 1024 1024" fill="none" role="img" aria-label="cadence">
      <defs>
        <linearGradient id="cadence-lit" x1="0" y1="0" x2="0.574" y2="0.819">
          <stop offset="0" stopColor="#2DD4BF" />
          <stop offset="1" stopColor="#22D3EE" />
        </linearGradient>
      </defs>
      <rect x="20" y="20" width="984" height="984" rx="214" fill="#0B0B0C" stroke="#2e3136" strokeWidth="40" />
      <path
        d="M 280 336 L 512 512 L 280 688"
        stroke="url(#cadence-lit)"
        strokeWidth="160"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <rect x="640" y="656" width="176" height="80" rx="40" fill="#2DD4BF" fillOpacity="0.8" />
    </svg>
  );
}
