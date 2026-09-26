import type { ReactNode } from "react";
import { countLabel, issueCounts } from "../lib/counts";
import type { ResourceState } from "../lib/cache";
import { NAV, type Route, type Screen } from "../lib/router";
import type { IssueCard, Project } from "../lib/types";
import Link from "./Link";
import { Logo } from "./Logo";

interface Props {
  screen: Screen;
  /** The href of a main-nav route (scope and drawer carried along). */
  navHref: (route: Route) => string;
  project: string;
  /** The current screen scoped to another project. */
  projectHref: (key: string) => string;
  projects: Project[];
  /** Counts derive from the cards (counts.ts), not `/api/projects`. */
  issues: ResourceState<IssueCard[]>;
  /** Set when the project list never loaded. */
  projectsError?: string | null;
  /** CAD-313: this browser's operator session — null while unknown. */
  signedIn?: boolean | null;
}

const boardIcon = (
  <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4">
    <rect x="1.5" y="2" width="3.6" height="12" rx="1" />
    <rect x="6.2" y="2" width="3.6" height="8" rx="1" />
    <rect x="10.9" y="2" width="3.6" height="10" rx="1" />
  </svg>
);
const homeIcon = (
  <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4">
    <circle cx="8" cy="8" r="5.5" />
    <path d="M8 5.5v3l2 1.4" />
  </svg>
);
const agentsIcon = (
  <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4">
    <rect x="2" y="3" width="12" height="8.5" rx="1.2" />
    <path d="M5.5 14h5" />
  </svg>
);
const appsIcon = (
  <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4">
    <path d="M8 1.8l5.2 3v6.4L8 14.2l-5.2-3V4.8l5.2-3z" />
    <path d="M2.8 4.8L8 7.8l5.2-3M8 7.8v6.4" />
  </svg>
);
const settingsIcon = (
  <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4">
    <circle cx="8" cy="8" r="2.2" />
    <path d="M8 1.8v1.6M8 12.6v1.6M1.8 8h1.6M12.6 8h1.6M3.4 3.4l1.1 1.1M11.5 11.5l1.1 1.1M12.6 3.4l-1.1 1.1M4.5 11.5l-1.1 1.1" />
  </svg>
);
const outboxIcon = (
  <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4">
    <path d="M2 5.5h12v8a1 1 0 01-1 1H3a1 1 0 01-1-1v-8z" />
    <path d="M2 5.5l2-3h8l2 3" />
    <path d="M6.5 8.5h3" />
  </svg>
);
const NAV_ICONS: Record<string, ReactNode> = {
  home: homeIcon,
  projects: boardIcon,
  apps: appsIcon,
  agents: agentsIcon,
  outbox: outboxIcon,
  settings: settingsIcon,
};

export default function Sidebar({ screen, navHref, project, projectHref, projects, issues, projectsError, signedIn = null }: Props) {
  // "…" until the cards load; a stale list keeps its numbers.
  const count = (key: string) => {
    if (!issues.data) return { n: issues.status === "failed" ? "!" : "…", title: issues.error ?? "loading issues" };
    const c = issueCounts(issues.data, key);
    return { n: String(c.open), title: countLabel(c) };
  };
  const all = count("all");
  return (
    <aside className="hidden lg:flex sticky top-0 h-screen flex-col border-r border-ink-700 bg-ink-875 px-[14px] pt-[22px] pb-4 overflow-y-auto">
      <div className="px-[7px] pb-[23px]">
        <span className="inline-flex items-center gap-2 text-ink-100 text-[17px] font-semibold tracking-[-.035em]">
          <Logo size={25} />
          <span>
            cadence<span className="text-ink-400 font-normal"> board</span>
          </span>
        </span>
      </div>
      <nav className="grid gap-[3px]">
        {NAV.map((item) => (
          <Link
            key={item.screen}
            href={navHref(item.route)}
            className="navlink"
            aria-current={screen === item.screen ? "page" : undefined}
          >
            {NAV_ICONS[item.screen]}
            {item.label}
          </Link>
        ))}
      </nav>
      <div className="slabel flex justify-between mx-[11px] mt-[22px] mb-[7px]">
        <span>Projects</span>
        <span className="text-[10px]">~/pm</span>
      </div>
      <div className="grid gap-[2px]">
        <Link href={projectHref("all")} className={`proj ${project === "all" ? "on" : ""}`}>
          <span className="truncate">All projects</span>
          <span className="num text-micro text-ink-500" title={all.title}>{all.n}</span>
        </Link>
        {projects.map((p) => (
          <Link
            key={p.key}
            href={projectHref(p.key)}
            className={`proj ${project === p.key ? "on" : ""}`}
          >
            <span className="truncate">{p.key}</span>
            <span className="num text-micro text-ink-500" title={count(p.key).title}>
              {p.prefix} {count(p.key).n}
            </span>
          </Link>
        ))}
        {projectsError && (
          <span className="mx-[11px] mt-1 text-micro text-fail" role="alert" title={projectsError}>
            could not load projects
          </span>
        )}
      </div>
      <div className="mt-auto pt-4 border-t border-ink-700 mx-1 text-ink-500 text-label leading-relaxed">
        <div className="slabel mb-1">session</div>
        {signedIn === true ? "operator · signed in" : signedIn === false ? "not signed in · read only" : "…"}
        <br />
        <span className="num text-micro">{location.host}</span>
      </div>
    </aside>
  );
}
