import "./appCover.css";

// Decorative artwork, not product screenshots or publisher verification.
const artwork = {
  checklist: (
    <>
      <g transform="translate(170 25) rotate(-7 130 135)">
        <rect
          width="255"
          height="276"
          rx="18"
          fill="var(--color-ink-850)"
          stroke="var(--color-ink-600)"
          strokeWidth="2"
        />
        <rect
          x="70"
          y="-7"
          width="116"
          height="28"
          rx="10"
          fill="var(--color-accent)"
        />
        <path
          d="M44 58h116"
          stroke="var(--color-ink-100)"
          strokeWidth="7"
          strokeLinecap="round"
        />
        <g fill="none" stroke="var(--color-accent)" strokeWidth="3">
          <rect x="30" y="94" width="27" height="27" rx="7" />
          <rect x="30" y="151" width="27" height="27" rx="7" />
          <rect x="30" y="208" width="27" height="27" rx="7" />
        </g>
        <g
          stroke="var(--color-accent)"
          strokeWidth="4"
          fill="none"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <path d="m37 107 6 6 12-14" />
          <path d="m37 164 6 6 12-14" />
        </g>
        <path
          d="M78 101h126m-126 14h84M78 158h105m-105 14h127M78 215h112m-112 14h76"
          fill="none"
          stroke="var(--color-ink-400)"
          strokeWidth="4"
          strokeLinecap="round"
        />
      </g>
      <g transform="translate(404 161) rotate(12)">
        <rect width="100" height="103" rx="18" fill="var(--color-accent)" />
        <path
          d="m27 51 16 16 31-35"
          fill="none"
          stroke="var(--color-on-accent)"
          strokeWidth="8"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      </g>
    </>
  ),
  schedule: (
    <>
      <g transform="translate(115 43) rotate(-3 180 120)">
        <rect
          width="365"
          height="234"
          rx="18"
          fill="var(--color-ink-850)"
          stroke="var(--color-ink-600)"
          strokeWidth="2"
        />
        <path
          d="M0 54h365M83 68v151M149 68v151M215 68v151M281 68v151M18 112h329M18 165h329"
          fill="none"
          stroke="var(--color-ink-700)"
          strokeWidth="2"
        />
        <path
          d="M75-9v26M290-9v26"
          stroke="var(--color-info)"
          strokeWidth="10"
          strokeLinecap="round"
        />
        <g fill="var(--color-ink-400)">
          <circle cx="41" cy="30" r="5" />
          <circle cx="59" cy="30" r="5" />
          <circle cx="77" cy="30" r="5" />
        </g>
        <g fill="var(--color-info)">
          <rect x="26" y="79" width="110" height="22" rx="6" />
          <rect x="161" y="132" width="111" height="22" rx="6" />
        </g>
        <g fill="var(--color-accent)">
          <rect x="226" y="79" width="110" height="22" rx="6" />
          <rect x="26" y="184" width="111" height="22" rx="6" />
        </g>
        <rect
          x="292"
          y="177"
          width="45"
          height="37"
          rx="7"
          fill="var(--color-ink-800)"
        />
      </g>
      <circle
        cx="483"
        cy="229"
        r="52"
        fill="var(--color-ink-850)"
        stroke="var(--color-info)"
        strokeWidth="5"
      />
      <path
        d="M483 198v32l22 12"
        fill="none"
        stroke="var(--color-ink-100)"
        strokeWidth="5"
        strokeLinecap="round"
      />
    </>
  ),
  notes: (
    <>
      <g transform="translate(137 59) rotate(-10 100 100)">
        <rect
          width="191"
          height="207"
          rx="12"
          fill="var(--color-ink-850)"
          stroke="var(--color-ink-600)"
          strokeWidth="2"
        />
        <rect
          x="25"
          y="33"
          width="98"
          height="8"
          rx="4"
          fill="var(--color-ink-200)"
        />
        <path
          d="M26 70h137M26 92h116M26 114h131"
          fill="none"
          stroke="var(--color-ink-400)"
          strokeWidth="4"
          strokeLinecap="round"
        />
        <circle cx="32" cy="167" r="8" fill="var(--color-accent)" />
      </g>
      <g transform="translate(289 31) rotate(8 112 121)">
        <path
          d="M0 13Q0 0 13 0h204q13 0 13 13v180l-50 50H13q-13 0-13-13Z"
          fill="color-mix(in srgb, var(--color-warn) 18%, var(--color-ink-850))"
          stroke="var(--color-warn)"
          strokeWidth="2"
        />
        <path
          d="M180 243v-38q0-12 12-12h38"
          fill="color-mix(in srgb, var(--color-warn) 30%, var(--color-ink-850))"
          stroke="var(--color-warn)"
          strokeWidth="2"
        />
        <path
          d="M30 58h137"
          stroke="var(--color-ink-100)"
          strokeWidth="7"
          strokeLinecap="round"
        />
        <path
          d="M30 92h156M30 115h131M30 138h151M30 161h105"
          fill="none"
          stroke="var(--color-ink-400)"
          strokeWidth="4"
          strokeLinecap="round"
        />
        <rect
          x="79"
          y="-12"
          width="73"
          height="26"
          rx="4"
          fill="var(--color-warn)"
        />
      </g>
    </>
  ),
  content: (
    <>
      <g transform="translate(148 34) rotate(-8 100 121)">
        <rect
          width="202"
          height="247"
          rx="16"
          fill="var(--color-ink-850)"
          stroke="var(--color-ink-600)"
          strokeWidth="2"
        />
        <rect
          x="16"
          y="17"
          width="170"
          height="151"
          rx="9"
          fill="color-mix(in srgb, var(--color-accent) 18%, var(--color-ink-850))"
        />
        <path
          d="M74 137c-21-44 0-70 25-80 2 28-5 56-25 80Zm5 3c41-2 68-26 72-56-37 5-65 23-72 56Z"
          fill="var(--color-accent)"
        />
        <path
          d="m72 153 42-76"
          stroke="var(--color-ink-200)"
          strokeWidth="3"
          strokeLinecap="round"
        />
        <path
          d="M23 192h132M23 211h99"
          fill="none"
          stroke="var(--color-ink-400)"
          strokeWidth="4"
          strokeLinecap="round"
        />
      </g>
      <g transform="translate(322 75) rotate(7 95 100)">
        <rect
          width="185"
          height="204"
          rx="16"
          fill="var(--color-ink-850)"
          stroke="var(--color-ink-600)"
          strokeWidth="2"
        />
        <circle cx="29" cy="31" r="12" fill="var(--color-accent)" />
        <path
          d="M54 26h90M54 38h62"
          fill="none"
          stroke="var(--color-ink-400)"
          strokeWidth="4"
          strokeLinecap="round"
        />
        <path
          d="M23 74h137M23 94h113M23 114h126"
          fill="none"
          stroke="var(--color-ink-300)"
          strokeWidth="4"
          strokeLinecap="round"
        />
        <rect
          x="21"
          y="151"
          width="143"
          height="29"
          rx="8"
          fill="var(--color-ink-800)"
        />
        <circle cx="143" cy="165" r="5" fill="var(--color-accent)" />
      </g>
      <g transform="translate(423 17)">
        <circle cx="34" cy="34" r="34" fill="var(--color-accent)" />
        <path
          d="m17 31 34-14-13 34-7-15-14-5Zm14 5 20-19"
          fill="none"
          stroke="var(--color-on-accent)"
          strokeWidth="3.5"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      </g>
    </>
  ),
  menu: (
    <>
      <g transform="translate(136 47) rotate(-5 170 112)">
        <rect
          width="340"
          height="230"
          rx="14"
          fill="var(--color-ink-850)"
          stroke="var(--color-ink-600)"
          strokeWidth="2"
        />
        <path d="M170 0v230" stroke="var(--color-ink-700)" strokeWidth="2" />
        <circle
          cx="85"
          cy="94"
          r="45"
          fill="color-mix(in srgb, var(--color-ok) 14%, var(--color-ink-850))"
          stroke="var(--color-ok)"
          strokeWidth="3"
        />
        <circle
          cx="85"
          cy="94"
          r="28"
          fill="none"
          stroke="var(--color-ok)"
          strokeWidth="2"
        />
        <path
          d="M69 109c-8-24 9-40 30-43-1 23-13 37-30 43Z"
          fill="var(--color-ok)"
        />
        <path
          d="M45 162h79M54 181h61"
          fill="none"
          stroke="var(--color-ink-400)"
          strokeWidth="4"
          strokeLinecap="round"
        />
        <path
          d="M197 43h112"
          stroke="var(--color-ink-100)"
          strokeWidth="7"
          strokeLinecap="round"
        />
        <path
          d="M197 80h112M197 95h82M197 125h112M197 140h90M197 170h112M197 185h73"
          fill="none"
          stroke="var(--color-ink-400)"
          strokeWidth="4"
          strokeLinecap="round"
        />
      </g>
      <g transform="translate(432 181) rotate(10)">
        <rect width="73" height="91" rx="10" fill="var(--color-ok)" />
        <path
          d="M18 29h37M18 46h37M18 63h23"
          fill="none"
          stroke="var(--color-on-accent)"
          strokeWidth="4"
          strokeLinecap="round"
        />
      </g>
    </>
  ),
};
const variants = ["checklist", "notes", "content"] as const;

function coverVariant(name: string, category: string): keyof typeof artwork {
  const purpose = `${category} ${name}`;
  if (/food|restaurant|menu/i.test(purpose)) return "menu";
  if (/schedule|booking|calendar|roster|shift/i.test(purpose))
    return "schedule";
  if (/social|marketing|content/i.test(purpose)) return "content";
  if (/knowledge|notes|wiki|customers/i.test(purpose)) return "notes";
  let hash = 0;
  for (let index = 0; index < name.length; index += 1)
    hash = (hash * 31 + name.charCodeAt(index)) >>> 0;
  return variants[hash % variants.length];
}

export default function AppCover({
  name,
  title,
  category = "",
}: {
  name: string;
  title: string;
  category?: string;
}) {
  const variant = coverVariant(name, category);
  return (
    <div className="app-cover" data-art={variant} aria-hidden="true">
      <svg viewBox="0 0 640 320" focusable="false">
        {artwork[variant]}
      </svg>
      <span className="app-cover-name">{title}</span>
    </div>
  );
}
