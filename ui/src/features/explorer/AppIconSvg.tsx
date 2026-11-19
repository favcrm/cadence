import { useId } from "react";

type IconKind =
  | "waba"
  | "accounting"
  | "hr"
  | "crm"
  | "social-content"
  | "meeting-minutes"
  | "designer-studio"
  | "checklist"
  | "catalog"
  | "calendar"
  | "fallback";

const FALLBACK_GRADIENTS: readonly [string, string][] = [
  ["#5b8def", "#2e54b8"],
  ["#22c6dc", "#0f766e"],
  ["#a78bfa", "#5b3fd6"],
  ["#f59e0b", "#b45309"],
  ["#34d399", "#0f766e"],
  ["#f472b6", "#be185d"],
];

function resolveIconKind(name: string, icon?: string): IconKind {
  const key = `${name} ${icon ?? ""}`.toLowerCase();
  if (/\b(waba|whatsapp)\b/.test(key)) return "waba";
  if (/\b(accounting|ledger|bookkeeping|finance|invoice)\b/.test(key)) return "accounting";
  if (/\b(hr|people|payroll|human-resources)\b/.test(key)) return "hr";
  if (/\b(crm|customers|contacts)\b/.test(key)) return "crm";
  if (/\b(social-content|social|instagram|marketing|content)\b/.test(key)) return "social-content";
  if (/\b(meeting-minutes|minutes|meeting|transcript)\b/.test(key)) return "meeting-minutes";
  if (/\b(designer-studio|designer|studio|creative|image)\b/.test(key)) return "designer-studio";
  if (/\b(calendar|schedule|booking)\b/.test(key)) return "calendar";
  if (/checklist|todo|task/.test(key)) return "checklist";
  if (/catalog|canary|package/.test(key)) return "catalog";
  return "fallback";
}

function hashString(text: string): number {
  let h = 0;
  for (let i = 0; i < text.length; i += 1) {
    h = (h * 31 + text.charCodeAt(i)) >>> 0;
  }
  return h;
}

/**
 * Bespoke macOS-squircle SVG app icons (48x48 viewBox).
 * Uses React useId() for gradient IDs so multiple sizes render safely on the same page.
 */
export function AppIconSvg({
  name,
  icon,
  title,
}: {
  name: string;
  icon?: string;
  title?: string;
}) {
  const uid = useId().replace(/:/g, "");
  const kind = resolveIconKind(`${name} ${title ?? ""}`, icon);
  const gradId = `cg-${uid}`;
  const sheenId = `cs-${uid}`;
  const label = (title?.trim() || name.trim() || "A");

  const frame = (c1: string, c2: string, sheenOpacity = 0.28) => (
    <>
      <defs>
        <linearGradient id={gradId} x1="6" y1="4" x2="42" y2="44" gradientUnits="userSpaceOnUse">
          <stop stopColor={c1} />
          <stop offset="1" stopColor={c2} />
        </linearGradient>
        <linearGradient id={sheenId} x1="24" y1="0" x2="24" y2="24" gradientUnits="userSpaceOnUse">
          <stop stopColor="#ffffff" stopOpacity={sheenOpacity} />
          <stop offset="1" stopColor="#ffffff" stopOpacity="0" />
        </linearGradient>
      </defs>
      <rect width="48" height="48" rx="12" fill={`url(#${gradId})`} />
      <rect x="0.75" y="0.75" width="46.5" height="46.5" rx="11.25" stroke={`url(#${sheenId})`} strokeWidth="1.5" />
    </>
  );

  let inner: React.ReactNode;
  switch (kind) {
    case "waba":
      inner = (
        <>
          {frame("#27d66b", "#118644")}
          <path
            d="M24 11.5c-6.9 0-12.5 5.2-12.5 11.7 0 2.4.8 4.7 2.2 6.6L12 36.5l7-1.8c1.6.7 3.3 1.1 5 1.1 6.9 0 12.5-5.2 12.5-11.7S30.9 11.5 24 11.5Z"
            fill="#ffffff"
            fillOpacity="0.95"
          />
          <path
            d="M18.8 23.6l3.4 3.4 7.2-7.3"
            stroke="#138c47"
            strokeWidth="2.9"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </>
      );
      break;
    case "accounting":
      inner = (
        <>
          {frame("#4172ea", "#1d3b8b", 0.26)}
          <rect x="12" y="11" width="24" height="26" rx="4.5" fill="#ffffff" fillOpacity="0.95" />
          <rect x="15.5" y="15" width="17" height="5" rx="2" fill="#2446a2" fillOpacity="0.16" />
          <path
            d="M16.5 25h6.5M16.5 29.5h4.5M16.5 33.5h6.5"
            stroke="#2446a2"
            strokeWidth="2.1"
            strokeLinecap="round"
          />
          <rect x="26" y="23.5" width="6.5" height="11" rx="2" fill="#f5b841" />
          <path d="M29.25 26v6" stroke="#ffffff" strokeWidth="1.8" strokeLinecap="round" />
        </>
      );
      break;
    case "hr":
      inner = (
        <>
          {frame("#f27d58", "#c6452d")}
          <circle cx="29.5" cy="18.5" r="4.2" fill="#ffffff" fillOpacity="0.68" />
          <path
            d="M24.5 32.5c.8-4 3.4-6.2 6.8-6.2 2.8 0 5 1.4 6 4.1a2 2 0 0 1-1.9 2.7h-10.9Z"
            fill="#ffffff"
            fillOpacity="0.68"
          />
          <circle cx="19.5" cy="17.5" r="5" fill="#ffffff" />
          <path
            d="M10.8 34.2c1.1-5.2 4.7-8.2 8.7-8.2s7.6 3 8.7 8.2a1.8 1.8 0 0 1-1.8 2.2H12.6a1.8 1.8 0 0 1-1.8-2.2Z"
            fill="#ffffff"
          />
        </>
      );
      break;
    case "crm":
      inner = (
        <>
          {frame("#38a6f7", "#2255d8")}
          <path
            d="M24 23l-9-5.5M24 23l9-5.5M24 23v10.5"
            stroke="#ffffff"
            strokeOpacity="0.55"
            strokeWidth="2.2"
            strokeLinecap="round"
          />
          <circle cx="14.5" cy="17" r="3.6" fill="#ffffff" fillOpacity="0.82" />
          <circle cx="33.5" cy="17" r="3.6" fill="#ffffff" fillOpacity="0.82" />
          <circle cx="24" cy="34.5" r="3.6" fill="#ffffff" fillOpacity="0.82" />
          <circle cx="24" cy="23" r="6" fill="#ffffff" />
          <circle cx="24" cy="23" r="2.4" fill="#2563eb" />
        </>
      );
      break;
    case "social-content":
      inner = (
        <>
          {frame("#df4ef0", "#6d35e6")}
          <rect x="11" y="13" width="22" height="22" rx="5.5" fill="#ffffff" fillOpacity="0.95" />
          <circle cx="17.5" cy="19.5" r="2.2" fill="#8b3cf0" />
          <path d="M13.5 31.2l5.6-6.2 3.8 3.6 3.2-3.4 4.4 6H13.5Z" fill="#a855f7" />
          <path d="M35.5 10.5l1.4 3.6 3.6 1.4-3.6 1.4-1.4 3.6-1.4-3.6-3.6-1.4 3.6-1.4 1.4-3.6Z" fill="#fde047" />
        </>
      );
      break;
    case "meeting-minutes":
      inner = (
        <>
          {frame("#f5a623", "#b85609")}
          <rect x="12" y="11" width="24" height="26" rx="5" fill="#ffffff" fillOpacity="0.95" />
          <path
            d="M17.5 19.5v-3M21.5 21.5v-7M25.5 20v-4M29.5 19v-2"
            stroke="#d97706"
            strokeWidth="2.4"
            strokeLinecap="round"
          />
          <path
            d="M17 26.5h14M17 31h9.5"
            stroke="#92400e"
            strokeOpacity="0.75"
            strokeWidth="2.2"
            strokeLinecap="round"
          />
        </>
      );
      break;
    case "designer-studio":
      inner = (
        <>
          {frame("#22c6dc", "#0f766e")}
          <path d="M24 10.5l10 15.5-5.5 10.5h-9L14 26l10-15.5Z" fill="#ffffff" fillOpacity="0.95" />
          <path d="M24 11.5v11.5" stroke="#0f766e" strokeWidth="2" strokeLinecap="round" />
          <circle cx="24" cy="24.8" r="2.6" fill="#0f766e" />
        </>
      );
      break;
    case "calendar":
      inner = (
        <>
          {frame("#ef4444", "#b91c1c")}
          <rect x="11.5" y="13" width="25" height="23.5" rx="4.5" fill="#ffffff" fillOpacity="0.95" />
          <path d="M11.5 19.5h25" stroke="#dc2626" strokeOpacity="0.25" strokeWidth="1.5" />
          <path d="M17.5 10.5v4.5M30.5 10.5v4.5" stroke="#ffffff" strokeWidth="2.4" strokeLinecap="round" />
          <rect x="16" y="23" width="5" height="4.5" rx="1.2" fill="#dc2626" />
          <rect x="23" y="23" width="5" height="4.5" rx="1.2" fill="#94a3b8" fillOpacity="0.45" />
          <rect x="16" y="29.5" width="5" height="4.5" rx="1.2" fill="#94a3b8" fillOpacity="0.45" />
          <rect x="23" y="29.5" width="5" height="4.5" rx="1.2" fill="#94a3b8" fillOpacity="0.45" />
        </>
      );
      break;
    case "checklist":
      inner = (
        <>
          {frame("#64748b", "#334155", 0.22)}
          <rect x="13" y="12" width="22" height="24" rx="4.5" fill="#ffffff" fillOpacity="0.93" />
          <path
            d="M17.5 19.5l2 2 3.8-4M17.5 27.5l2 2 3.8-4"
            stroke="#16a34a"
            strokeWidth="2.2"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
          <path d="M26 19.5h5.2M26 27.5h5.2" stroke="#475569" strokeWidth="2.2" strokeLinecap="round" />
        </>
      );
      break;
    case "catalog":
      inner = (
        <>
          {frame("#818cf8", "#4338ca", 0.26)}
          <path d="M24 11.5L35 17.8v12.4L24 36.5 13 30.2V17.8L24 11.5Z" fill="#ffffff" fillOpacity="0.94" />
          <path d="M24 24l11-6.2M24 24l-11-6.2M24 24v12.5" stroke="#4338ca" strokeWidth="2" strokeLinejoin="round" />
          <circle cx="35.5" cy="12.5" r="3.5" fill="#fbbf24" stroke="#4338ca" strokeWidth="1.5" />
        </>
      );
      break;
    default: {
      const [c1, c2] = FALLBACK_GRADIENTS[hashString(name || label) % FALLBACK_GRADIENTS.length];
      inner = (
        <>
          {frame(c1, c2, 0.24)}
          <rect x="13" y="13" width="9" height="9" rx="2.5" fill="#ffffff" fillOpacity="0.95" />
          <rect x="26" y="13" width="9" height="9" rx="2.5" fill="#ffffff" fillOpacity="0.65" />
          <rect x="13" y="26" width="9" height="9" rx="2.5" fill="#ffffff" fillOpacity="0.65" />
          <rect x="26" y="26" width="9" height="9" rx="2.5" fill="#ffffff" fillOpacity="0.95" />
        </>
      );
    }
  }

  return (
    <svg viewBox="0 0 48 48" width="100%" height="100%" fill="none" focusable="false" aria-hidden="true">
      {inner}
    </svg>
  );
}
