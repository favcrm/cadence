/** CAD-1320: the catalog's per-agent face — a deterministic "colleague
 * bean" avatar. `lookFor(alias)` hashes the alias onto one of the look
 * palettes, so a given agent always wears the same look. The avatar's
 * palette colours are artwork, not UI chrome: they live here rather than
 * in the ink/semantic tokens (which describe surfaces and state). Blink,
 * bob and accessory motion are CSS (agents.css); `active` picks the
 * livelier loop and `still` freezes every animation. */

import { useId } from "react";

export const AVATAR_LOOKS = [
  "builder",
  "guard",
  "bookkeeper",
  "content",
  "designer",
  "reception",
  "operations",
  "support",
  "tester",
  "robot",
  "custom",
] as const;
export type AvatarLook = (typeof AVATAR_LOOKS)[number];

type Palette = {
  bg: string;
  body: string;
  shade: string;
  ink: string;
  accent: string;
  prop: string;
  propLight: string;
};

const PAL: Record<AvatarLook, Palette> = {
  support: { bg: "#E6FAF4", body: "#8FE4CC", shade: "#5CC2A8", ink: "#0E3A2B", accent: "#FF8CA5", prop: "#0E3A2B", propLight: "#FFFFFF" },
  operations: { bg: "#FFF4E0", body: "#F5C98B", shade: "#E0A85C", ink: "#4A2506", accent: "#FF7E6B", prop: "#4A2506", propLight: "#FFFFFF" },
  bookkeeper: { bg: "#E9F1E5", body: "#A9D494", shade: "#7EB56A", ink: "#1F3A13", accent: "#FF8CA5", prop: "#1F3A13", propLight: "#FFFFFF" },
  content: { bg: "#FFEEF4", body: "#F7A9BF", shade: "#E07D97", ink: "#4E0A24", accent: "#FFE07D", prop: "#4E0A24", propLight: "#FFFFFF" },
  reception: { bg: "#E5F4FF", body: "#9BC9F2", shade: "#6BA4DC", ink: "#0A2A55", accent: "#FF7BA8", prop: "#0A2A55", propLight: "#FFFFFF" },
  builder: { bg: "#FFF6D6", body: "#FFD66B", shade: "#E8B33D", ink: "#3D2A00", accent: "#FF7E6B", prop: "#3D2A00", propLight: "#FFFFFF" },
  guard: { bg: "#FFECEC", body: "#F7A1A1", shade: "#E07575", ink: "#4A0F0F", accent: "#FFD66B", prop: "#4A0F0F", propLight: "#FFFFFF" },
  tester: { bg: "#E3F7FB", body: "#8ED8E8", shade: "#5BB7CC", ink: "#0B3440", accent: "#FF8CA5", prop: "#0B3440", propLight: "#FFFFFF" },
  designer: { bg: "#FFF0E6", body: "#FFB98A", shade: "#EE9460", ink: "#4A1F05", accent: "#6BA4DC", prop: "#4A1F05", propLight: "#FFFFFF" },
  robot: { bg: "#EEF0F3", body: "#C9CED6", shade: "#A3AAB5", ink: "#1F2430", accent: "#2DD4BF", prop: "#1F2430", propLight: "#FFFFFF" },
  custom: { bg: "#F3F3F5", body: "#E8E6EE", shade: "#C4C1D3", ink: "#2C2840", accent: "#8B5CF6", prop: "#2C2840", propLight: "#FFFFFF" },
};

/** Stable alias → look assignment: same agent, same face. */
export function lookFor(alias: string): AvatarLook {
  let hash = 0;
  for (const ch of alias) hash = (hash * 31 + ch.charCodeAt(0)) >>> 0;
  return AVATAR_LOOKS[hash % AVATAR_LOOKS.length];
}

function Mouth({ slug, p }: { slug: AvatarLook; p: Palette }) {
  const stroke = {
    stroke: p.ink,
    strokeWidth: 1.8,
    strokeLinecap: "round" as const,
    fill: "none",
  };
  if (slug === "operations")
    return <line x1="45" y1="57" x2="51" y2="57" {...stroke} />;
  if (slug === "robot" || slug === "custom")
    return <line x1="45" y1="56" x2="51" y2="56" {...stroke} />;
  if (slug === "content")
    return <ellipse cx="48" cy="57" rx="2" ry="2.2" fill={p.ink} />;
  return <path d="M43 55 Q48 60 53 55" {...stroke} />;
}

function Prop({ slug, p }: { slug: AvatarLook; p: Palette }) {
  switch (slug) {
    case "builder":
      return (
        <g>
          <path d="M27 31 Q27 13 48 13 Q69 13 69 31 Z" fill="#FF9F43" />
          <rect x="45" y="13" width="6" height="17" rx="2" fill="#FFB866" />
          <rect x="21" y="29" width="54" height="5" rx="2.5" fill="#E8822A" />
          <g className="tool">
            <g transform="rotate(30 80 56)">
              <rect x="78" y="48" width="4" height="18" rx="2" fill={p.prop} />
              <circle cx="80" cy="46" r="5.5" fill={p.prop} />
              <rect x="78.2" y="39" width="3.6" height="6" fill={p.bg} />
            </g>
          </g>
        </g>
      );
    case "guard":
      return (
        <g>
          <path d="M28 30 Q48 12 68 30 Q48 25 28 30 Z" fill={p.prop} />
          <g className="shield">
            <path d="M80 37 L90 41 L90 51 Q90 59 80 63 Q70 59 70 51 L70 41 Z" fill={p.prop} />
            <path d="M75 50 L79 54 L86 46" stroke={p.accent} strokeWidth="2.2" fill="none" strokeLinecap="round" strokeLinejoin="round" />
          </g>
        </g>
      );
    case "robot":
      return (
        <g>
          <line x1="48" y1="24" x2="48" y2="14" stroke={p.prop} strokeWidth="2" />
          <circle className="bulb" cx="48" cy="11.5" r="3.5" fill={p.accent} />
          <rect x="19" y="47" width="5" height="9" rx="1.5" fill={p.prop} />
          <rect x="72" y="47" width="5" height="9" rx="1.5" fill={p.prop} />
        </g>
      );
    case "operations":
      return (
        <g className="sig-ops">
          <path d="M28 20 Q48 8 68 20 Q68 24 48 24 Q28 24 28 20 Z" fill={p.prop} />
          <rect x="38" y="58" width="20" height="16" rx="2" fill={p.propLight} stroke={p.prop} strokeWidth="1.5" />
          <rect x="42" y="56" width="12" height="4" rx="1.5" fill={p.prop} />
          <line x1="41" y1="64" x2="55" y2="64" stroke={p.prop} strokeWidth="1" />
        </g>
      );
    case "reception":
      return (
        <g>
          <g className="bell">
            <path d="M12 72 Q12 62 20 62 Q28 62 28 72 Z" fill={p.prop} />
            <rect x="10" y="72" width="20" height="2.5" rx="1" fill={p.prop} />
            <circle cx="20" cy="60" r="1.8" fill={p.accent} />
          </g>
        </g>
      );
    case "support":
      return (
        <g className="sig-support">
          <path d="M72 16 Q82 16 82 26 Q82 34 74 34 L70 40 L70 34 Q64 32 64 26 Q64 16 72 16 Z" fill={p.prop} />
          <path d="M70 22 Q72 20 73 22 Q74 20 76 22 Q76 26 73 28 Q70 26 70 22 Z" fill={p.accent} />
        </g>
      );
    case "tester":
      return (
        <g>
          <g className="phone">
            <rect x="73" y="33" width="14" height="23" rx="3" fill={p.prop} />
            <rect x="75" y="36" width="10" height="15" rx="1" fill={p.accent} opacity=".9" />
          </g>
          <path className="spark" d="M12 34 l3 3 l7 -8" stroke={p.accent} strokeWidth="2.2" fill="none" strokeLinecap="round" strokeLinejoin="round" />
        </g>
      );
    case "designer":
      return (
        <g>
          <path d="M27 26 Q34 10 56 13 Q70 16 67 25 Q50 21 27 28 Z" fill={p.prop} />
          <g className="brush">
            <rect x="76" y="36" width="3" height="18" rx="1.5" fill={p.prop} transform="rotate(-25 77 45)" />
            <path d="M80.5 31 q4 -2 5 2 q-2 3 -5 2 z" fill={p.accent} />
          </g>
        </g>
      );
    case "bookkeeper":
      return (
        <g>
          <circle cx="40" cy="46" r="6" fill="none" stroke={p.prop} strokeWidth="2" />
          <circle cx="56" cy="46" r="6" fill="none" stroke={p.prop} strokeWidth="2" />
          <line x1="46" y1="46" x2="50" y2="46" stroke={p.prop} strokeWidth="2" />
          <g className="ledger">
            <rect x="74" y="56" width="12" height="10" rx="1.5" fill={p.prop} />
            <rect x="74" y="56" width="6" height="10" fill={p.shade} opacity=".3" />
          </g>
        </g>
      );
    case "content":
      return (
        <g>
          <g className="sprout">
            <path d="M48 22 Q42 14 38 18 Q40 24 48 24 Z" fill={p.shade} />
            <path d="M48 22 Q54 12 58 18 Q54 24 48 24 Z" fill={p.body} />
          </g>
        </g>
      );
    default:
      return null;
  }
}

export default function AgentAvatar({
  slug,
  active = false,
  still = false,
  size = 58,
  title,
}: {
  slug: AvatarLook;
  /** Livelier bob for a working agent. */
  active?: boolean;
  /** Freeze blink/bob/accessories for a dead or parked agent. */
  still?: boolean;
  size?: number;
  /** Accessible label when the avatar carries meaning on its own. */
  title?: string;
}) {
  const p = PAL[slug] ?? PAL.custom;
  const raw = useId().replace(/[^a-zA-Z0-9]/g, "");
  const blinkDelay = [...slug].reduce((n, c) => n + c.charCodeAt(0), 0);
  return (
    <span
      className={`agent-avatar${active ? " is-active" : ""}${still ? " still" : ""}`}
      data-slug={slug}
      style={
        {
          width: size,
          height: size,
          "--blink-delay": `${(blinkDelay % 40) / 10}s`,
          "--bob-delay": `${(blinkDelay % 25) / 10}s`,
        } as React.CSSProperties
      }
    >
      <svg
        viewBox="0 0 96 96"
        role={title ? "img" : undefined}
        aria-label={title}
        aria-hidden={title ? undefined : true}
      >
        <defs>
          <radialGradient id={`bga-${raw}`} cx="32%" cy="28%" r="85%">
            <stop offset="0%" stopColor={p.bg} />
            <stop offset="100%" stopColor={p.bg} stopOpacity=".55" />
          </radialGradient>
          <linearGradient id={`bgb-${raw}`} x1="0" x2="0" y1="0" y2="1">
            <stop offset="0%" stopColor={p.body} />
            <stop offset="100%" stopColor={p.shade} />
          </linearGradient>
        </defs>
        <circle cx="48" cy="48" r="46" fill={`url(#bga-${raw})`} />
        <g className="bean-group">
          <ellipse cx="48" cy="82" rx="18" ry="2.5" fill={p.ink} opacity=".08" />
          <g className="bean-body">
            <path d="M24 52 Q24 24 48 24 Q72 24 72 52 Q72 78 48 78 Q24 78 24 52 Z" fill={`url(#bgb-${raw})`} />
            <ellipse cx="38" cy="36" rx="9" ry="6" fill="#FFFFFF" opacity=".35" />
            <ellipse cx="24" cy="58" rx="4" ry="5" fill={p.shade} />
            <ellipse cx="72" cy="58" rx="4" ry="5" fill={p.shade} />
            <ellipse cx="40" cy="78" rx="5" ry="3" fill={p.shade} />
            <ellipse cx="56" cy="78" rx="5" ry="3" fill={p.shade} />
            <circle cx="32" cy="52" r="4" fill={p.accent} opacity=".45" />
            <circle cx="64" cy="52" r="4" fill={p.accent} opacity=".45" />
            <g className="eyes">
              <g>
                <ellipse cx="40" cy="46" rx="3.2" ry="4" fill={p.ink} />
                <circle cx="41.2" cy="44.6" r="1.1" fill="#FFFFFF" />
              </g>
              <g>
                <ellipse cx="56" cy="46" rx="3.2" ry="4" fill={p.ink} />
                <circle cx="57.2" cy="44.6" r="1.1" fill="#FFFFFF" />
              </g>
            </g>
            <Mouth slug={slug} p={p} />
          </g>
          <Prop slug={slug} p={p} />
        </g>
      </svg>
    </span>
  );
}
