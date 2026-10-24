import type { ReactNode } from "react";
import Link from "../../ui/Link";
import { useTranslate } from "../../lib/locale";
import "./settings-layout.css";

export type SettingsNavItem = { label: string; href: string; on: boolean };
export type SettingsNavGroup = { label: string; items: SettingsNavItem[] };

/**
 * The Settings shell: a compact sub-navigation beside the section content.
 * The app sidebar stays; this rail only organises Settings. It replaces the
 * former horizontal section strip so a growing Settings surface (Connections,
 * Email sending, Status) does not compete for one row.
 */
export default function SettingsLayout({
  groups,
  children,
}: {
  groups: SettingsNavGroup[];
  children: ReactNode;
}) {
  const t = useTranslate();
  return (
    <div className="settings-layout">
      <nav className="settings-subnav" aria-label={t("Settings sections")}>
        {groups.map((group) => (
          <div className="settings-subnav-group" key={group.label}>
            <p className="settings-subnav-label">{t(group.label)}</p>
            {group.items.map((item) => (
              <Link
                key={item.label}
                href={item.href}
                className="settings-subnav-link"
                aria-current={item.on ? "page" : undefined}
                data-on={item.on ? "true" : undefined}
              >
                {t(item.label)}
              </Link>
            ))}
          </div>
        ))}
      </nav>
      <div className="settings-content">{children}</div>
    </div>
  );
}
