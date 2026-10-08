/**
 * The Explorer's shared bits (CAD-1129): the app glyph, trust chip and
 * install-state chip every screen reuses — so a card, the detail hero
 * and the home tile read the same way.
 */

/** A monogram tile — the app's first letter on a tinted tile, or its
 * `listing.icon` name as the label's shape. */
export function AppGlyph({ name, icon, size }: { name: string; icon?: string; size?: "sm" | "lg" }) {
  const letter = (name.trim()[0] ?? "A").toUpperCase();
  const cls = size === "lg" ? "w-16 h-16 rounded-2xl text-2xl" : size === "sm" ? "w-9 h-9 rounded-lg text-cardtitle" : "w-11 h-11 rounded-xl text-xl";
  return (
    <span
      aria-hidden
      title={icon ? `${name} · ${icon}` : name}
      className={`shrink-0 grid place-items-center bg-accent/15 text-accent font-semibold ${cls}`}
    >
      {letter}
    </span>
  );
}

/** Cadence-shipped vs an unreviewed Git app — the trust chip. */
export function TrustChip({ trust }: { trust?: "cadence" | "unreviewed" }) {
  if (trust === "cadence") return <span className="chip ok">Cadence</span>;
  return <span className="chip warn">Not reviewed by Cadence</span>;
}

/** Installed / off / requested / removed — the state a card wears. */
export function InstallStateChip({ state }: { state?: string }) {
  switch (state) {
    case "installed":
      return <span className="chip ok">Installed</span>;
    case "off":
      return <span className="chip">Access off</span>;
    case "requested":
      return <span className="chip">Requested</span>;
    case "removed":
      return <span className="chip">Removed</span>;
    default:
      return null;
  }
}
