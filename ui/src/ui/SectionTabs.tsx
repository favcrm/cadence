import Link from "./Link";
import { useTranslate } from "../lib/locale";

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
  const t = useTranslate();
  // `label` is the caller's name (an authored project slug or a UI word); only
  // the "{label} sections" wording is translated, the name is inserted as given.
  return (
    <nav className={`section-nav${bare ? "" : " px-4 lg:px-8"}`} aria-label={t("{label} sections").replace("{label}", () => label)}>
      {tabs.map((tab) => (
        <Link
          key={tab.label}
          href={tab.href}
          aria-current={tab.on ? "page" : undefined}
          className="section-nav-link"
        >
          {t(tab.label)}
        </Link>
      ))}
    </nav>
  );
}
