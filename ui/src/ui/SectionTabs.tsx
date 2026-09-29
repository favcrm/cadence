import Link from "./Link";

/** Page navigation: real links with a visible current-page indicator. */
export default function SectionTabs({
  label,
  tabs,
  bare,
}: {
  label: string;
  tabs: { label: string; href: string; on: boolean }[];
  /** Embedded contexts (like the App shell outlet) drop the page
   *  padding; full-bleed pages keep it. Styling stays the shared
   *  `section-nav` kit either way. */
  bare?: boolean;
}) {
  return (
    <nav className={`section-nav${bare ? "" : " px-4 lg:px-8"}`} aria-label={`${label} sections`}>
      {tabs.map((t) => (
        <Link
          key={t.label}
          href={t.href}
          aria-current={t.on ? "page" : undefined}
          className="section-nav-link"
        >
          {t.label}
        </Link>
      ))}
    </nav>
  );
}
