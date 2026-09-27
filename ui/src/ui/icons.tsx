/** Hugeicons Stroke Rounded — the board's one icon vocabulary, shared
 *  with AgenticOS v2 Studio. Every glyph is drawn the same way: 1.7
 *  stroke, currentColor, aria-hidden (the label lives on the control),
 *  never squashed by flex. Sizes match the inline SVGs they replace. */
import { HugeiconsIcon } from "@hugeicons/react";
import type { ComponentProps, CSSProperties } from "react";
import {
  Alert02Icon,
  ArrowDown01Icon,
  ArrowRight01Icon,
  Cancel01Icon,
  CircleArrowUp01Icon,
  ContrastIcon,
  ConnectIcon,
  CubeIcon,
  DashboardSquare01Icon,
  File01Icon,
  Folder01Icon,
  Home01Icon,
  KanbanIcon,
  MailSend01Icon,
  Menu01Icon,
  Moon02Icon,
  MoreHorizontalIcon,
  Pulse01Icon,
  RefreshIcon,
  Robot01Icon,
  Search01Icon,
  Settings02Icon,
  SquareLock01Icon,
  Sun01Icon,
  Tick02Icon,
  Upload01Icon,
} from "@hugeicons/core-free-icons";

/** What a semantic icon takes: the size (each export has the board's
 *  default) plus the class/style a call site positions it with. */
type IconProps = { size?: number; className?: string; style?: CSSProperties };

function Glyph({
  icon,
  size = 15,
  className,
  style,
}: Pick<ComponentProps<typeof HugeiconsIcon>, "icon" | "className" | "style"> & {
  size?: number | undefined;
}) {
  return (
    <HugeiconsIcon
      icon={icon}
      size={size}
      strokeWidth={1.7}
      color="currentColor"
      aria-hidden="true"
      focusable="false"
      className={className}
      style={{ flexShrink: 0, ...style }}
    />
  );
}

/** Home — the master chat and the needs-you rail. */
export const IconHome = ({ size = 15, ...rest }: IconProps = {}) => (
  <Glyph icon={Home01Icon} size={size} {...rest} />
);
/** Projects — the kanban board. */
export const IconProjects = ({ size = 15, ...rest }: IconProps = {}) => (
  <Glyph icon={KanbanIcon} size={size} {...rest} />
);
/** Apps — the installed app bundles. */
export const IconApps = ({ size = 15, ...rest }: IconProps = {}) => (
  <Glyph icon={CubeIcon} size={size} {...rest} />
);
/** Agents — panes, models and their bound work. */
export const IconAgents = ({ size = 15, ...rest }: IconProps = {}) => (
  <Glyph icon={Robot01Icon} size={size} {...rest} />
);
/** Outbox — published local items. */
export const IconOutbox = ({ size = 15, ...rest }: IconProps = {}) => (
  <Glyph icon={MailSend01Icon} size={size} {...rest} />
);
/** Settings — models, memory and the update card. */
export const IconSettings = ({ size = 15, ...rest }: IconProps = {}) => (
  <Glyph icon={Settings02Icon} size={size} {...rest} />
);
/** Wiki — the page store, and a page or file in the tree. */
export const IconWiki = ({ size = 15, ...rest }: IconProps = {}) => (
  <Glyph icon={File01Icon} size={size} {...rest} />
);
/** A folder in the wiki tree and its empty state. */
export const IconFolder = ({ size = 14, ...rest }: IconProps = {}) => (
  <Glyph icon={Folder01Icon} size={size} {...rest} />
);
/** The disclosure chevron; rotate it for open rows. */
export const IconChevron = ({ size = 10, ...rest }: IconProps = {}) => (
  <Glyph icon={ArrowDown01Icon} size={size} {...rest} />
);
/** Tree disclosure; the row rotates it when the folder is open. */
export const IconCaret = ({ size = 10, ...rest }: IconProps = {}) => (
  <Glyph icon={ArrowRight01Icon} size={size} {...rest} />
);
/** Close a drawer or a panel. */
export const IconClose = ({ size = 12, ...rest }: IconProps = {}) => (
  <Glyph icon={Cancel01Icon} size={size} {...rest} />
);
/** The overflow "more" menu. */
export const IconMore = ({ size = 15, ...rest }: IconProps = {}) => (
  <Glyph icon={MoreHorizontalIcon} size={size} {...rest} />
);
/** Search. */
export const IconSearch = ({ size = 15, ...rest }: IconProps = {}) => (
  <Glyph icon={Search01Icon} size={size} {...rest} />
);
/** The read-only chip's lock. */
export const IconLock = ({ size = 12, ...rest }: IconProps = {}) => (
  <Glyph icon={SquareLock01Icon} size={size} {...rest} />
);
/** The daemon is reachable — a live pulse. */
export const IconPulse = ({ size = 12, ...rest }: IconProps = {}) => (
  <Glyph icon={Pulse01Icon} size={size} {...rest} />
);
/** The daemon is unreachable — a warning. */
export const IconWarning = ({ size = 12, ...rest }: IconProps = {}) => (
  <Glyph icon={Alert02Icon} size={size} {...rest} />
);
/** Re-read the folders. */
export const IconRefresh = ({ size = 12, ...rest }: IconProps = {}) => (
  <Glyph icon={RefreshIcon} size={size} {...rest} />
);
/** Theme: follow the system. */
export const IconTheme = ({ size = 14, ...rest }: IconProps = {}) => (
  <Glyph icon={ContrastIcon} size={size} {...rest} />
);
/** Theme: light. */
export const IconSun = ({ size = 14, ...rest }: IconProps = {}) => (
  <Glyph icon={Sun01Icon} size={size} {...rest} />
);
/** Theme: dark. */
export const IconMoon = ({ size = 14, ...rest }: IconProps = {}) => (
  <Glyph icon={Moon02Icon} size={size} {...rest} />
);
/** Drop files into the wiki. */
export const IconUpload = ({ size = 34, ...rest }: IconProps = {}) => (
  <Glyph icon={Upload01Icon} size={size} {...rest} />
);
/** A completed upload. */
export const IconCheck = ({ size = 13, ...rest }: IconProps = {}) => (
  <Glyph icon={Tick02Icon} size={size} {...rest} />
);
/** Folder view: grid. */
export const IconGrid = ({ size = 12, ...rest }: IconProps = {}) => (
  <Glyph icon={DashboardSquare01Icon} size={size} {...rest} />
);
/** Folder view: list. */
export const IconList = ({ size = 12, ...rest }: IconProps = {}) => (
  <Glyph icon={Menu01Icon} size={size} {...rest} />
);

/** A newer UI version is available. */
export const IconUpdate = ({ size = 18, ...rest }: IconProps = {}) => (
  <Glyph icon={CircleArrowUp01Icon} size={size} {...rest} />
);

/** Daemon connection — paired plugs, distinct from activity and refresh. */
export const IconConnection = ({ size = 18, ...rest }: IconProps = {}) => (
  <Glyph icon={ConnectIcon} size={size} {...rest} />
);
