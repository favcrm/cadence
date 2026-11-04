import type { ReactNode } from "react";
import { HugeiconsIcon } from "@hugeicons/react";
import { BrainIcon, GlobalIcon, Key01Icon, Mail01Icon, Shield01Icon, SlidersHorizontalIcon, UserIcon } from "@hugeicons/core-free-icons";
import Link from "../../ui/Link";
import { IconAgents, IconConnection, IconPulse } from "../../ui/icons";
import { useTranslate } from "../../lib/locale";
import type { SettingsIcon } from "./settingsNav";
import "./settings-layout.css";

export type SettingsNavItem = { label: string; href: string; on: boolean; icon: SettingsIcon };
export type SettingsNavGroup = { label: string; items: SettingsNavItem[] };

const line = (icon: typeof Mail01Icon) => <HugeiconsIcon icon={icon} size={16} strokeWidth={1.7} aria-hidden="true" />;

const ICONS: Record<SettingsIcon, ReactNode> = {
  preferences: line(SlidersHorizontalIcon),
  connections: <IconConnection size={16} />,
  email: line(Mail01Icon),
  general: line(GlobalIcon),
  status: <IconPulse size={16} />,
  agents: <IconAgents size={16} />,
  memory: line(BrainIcon),
  permissions: line(Shield01Icon),
  custody: line(Key01Icon),
  account: line(UserIcon),
};

/**
 * The Settings shell: a second menu beside the content, in the primary menu's
 * own `.navlink` style so the two read as one system.
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
        <p className="settings-subnav-title">{t("Settings")}</p>
        {groups.map((group) => (
          <div className="settings-subnav-group" key={group.label}>
            <p className="settings-subnav-label">{t(group.label)}</p>
            {group.items.map((item) => (
              <Link
                key={item.label}
                href={item.href}
                className="navlink"
                aria-current={item.on ? "page" : undefined}
              >
                <span className="settings-subnav-icon" aria-hidden="true">{ICONS[item.icon]}</span>
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
