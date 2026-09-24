import Link from "./Link";

/** A screen's sub-sections as links (Projects → issues / context, Settings → models / memory). */
export default function SectionTabs({
  label,
  tabs,
}: {
  label: string;
  tabs: { label: string; href: string; on: boolean }[];
}) {
  return (
    <nav className="px-4 lg:px-8 pt-4 flex items-center gap-1.5 min-w-0" aria-label={`${label} sections`}>
      <span className="kicker truncate mr-1.5">{label}</span>
      {tabs.map((t) => (
        <Link
          key={t.label}
          href={t.href}
          replace
          aria-current={t.on ? "page" : undefined}
          className={`h-7 inline-flex items-center px-2.5 rounded text-label ${
            t.on ? "bg-accent/15 text-accent font-medium" : "text-ink-400 hover:bg-ink-800 hover:text-ink-100"
          }`}
        >
          {t.label}
        </Link>
      ))}
    </nav>
  );
}
