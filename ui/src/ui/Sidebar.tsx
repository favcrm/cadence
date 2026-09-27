import type { ReactNode } from "react";
import { countLabel, issueCounts } from "../lib/counts";
import type { ResourceState } from "../lib/cache";
import { NAV, type Route, type Screen } from "../lib/router";
import type { IssueCard, Project, Meta } from "../lib/types";
import Link from "./Link";
import { Logo } from "./Logo";
import {
  IconAgents,
  IconApps,
  IconHome,
  IconOutbox,
  IconProjects,
  IconSettings,
  IconWiki,
} from "./icons";

interface Props {
  screen: Screen;
  /** The href of a main-nav route (scope and drawer carried along). */
  navHref: (route: Route) => string;
  /** The project page on screen, or null when this screen is not one. */
  project: string | null;
  /** Opens that project's page. Never filters the current screen. */
  projectHref: (key: string) => string;
  projects: Project[];
  /** Counts derive from the cards (counts.ts), not `/api/projects`. */
  issues: ResourceState<IssueCard[]>;
  /** Set when the project list never loaded. */
  projectsError?: string | null;
  /** CAD-313: this browser's operator session — null while unknown. */
  signedIn?: boolean | null;
  sessionUser?: NonNullable<Meta["session"]>["user"];
}

const NAV_ICONS: Record<string, ReactNode> = {
  home: <IconHome />,
  projects: <IconProjects />,
  wiki: <IconWiki />,
  apps: <IconApps />,
  agents: <IconAgents />,
  outbox: <IconOutbox />,
  settings: <IconSettings />,
};

export default function Sidebar({ screen, navHref, project, projectHref, projects, issues, projectsError, signedIn = null, sessionUser }: Props) {
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
        {signedIn === true ? (
          <div title={sessionUser?.email}>
            <div className="truncate text-ink-300">{sessionUser?.name || sessionUser?.email || "operator"}</div>
            {sessionUser && <div className="truncate text-micro">{sessionUser.email}</div>}
            <div>{sessionUser?.role || "operator"} · signed in</div>
          </div>
        ) : signedIn === false ? "not signed in · read only" : "…"}
        <br />
        <span className="num text-micro">{location.host}</span>
      </div>
    </aside>
  );
}
