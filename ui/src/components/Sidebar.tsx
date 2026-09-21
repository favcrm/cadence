import type { Project } from "../types";
import { Logo } from "./Logo";

interface Props {
  tab: string;
  onTab: (tab: "overview" | "board" | "plan" | "agents" | "memory") => void;
  project: string;
  onProject: (key: string) => void;
  projects: Project[];
  total: number;
}

const boardIcon = (
  <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4">
    <rect x="1.5" y="2" width="3.6" height="12" rx="1" />
    <rect x="6.2" y="2" width="3.6" height="8" rx="1" />
    <rect x="10.9" y="2" width="3.6" height="10" rx="1" />
  </svg>
);
const planIcon = (
  <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4">
    <path d="M3.5 1.8h6l3 3v9.4h-9z" />
    <path d="M5.8 7.5h4.4M5.8 10h4.4" />
  </svg>
);
const loopsIcon = (
  <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4">
    <circle cx="3.5" cy="8" r="1.7" />
    <circle cx="12.5" cy="3.5" r="1.7" />
    <circle cx="12.5" cy="12.5" r="1.7" />
    <path d="M5 7.2l6-3M5 8.8l6 3" />
  </svg>
);
const overviewIcon = (
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
const memoryIcon = (
  <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4">
    <path d="M8 2.2c-2.6 0-4.7 1.9-4.7 4.3 0 1.4.7 2.7 1.7 3.5.4.4.7.9.7 1.5v1.3c0 .6.5 1 1 1h2.6c.6 0 1-.4 1-1v-1.3c0-.6.2-1.1.7-1.5 1-.8 1.7-2.1 1.7-3.5 0-2.4-2.1-4.3-4.7-4.3z" />
    <path d="M6.5 8.5h3" />
  </svg>
);

export default function Sidebar({ tab, onTab, project, onProject, projects, total }: Props) {
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
        <button
          onClick={() => onTab("overview")}
          className="navlink"
          aria-current={tab === "overview" ? "page" : undefined}
        >
          {overviewIcon}Overview
        </button>
        <button
          onClick={() => onTab("board")}
          className="navlink"
          aria-current={tab === "board" ? "page" : undefined}
        >
          {boardIcon}Projects
        </button>
        <button
          onClick={() => onTab("plan")}
          className="navlink"
          aria-current={tab === "plan" ? "page" : undefined}
        >
          {planIcon}Plan
        </button>
        <button
          onClick={() => onTab("agents")}
          className="navlink"
          aria-current={tab === "agents" ? "page" : undefined}
        >
          {agentsIcon}Agents
        </button>
        <button
          onClick={() => onTab("memory")}
          className="navlink"
          aria-current={tab === "memory" ? "page" : undefined}
        >
          {memoryIcon}Memory
        </button>
        <span className="navlink opacity-45 cursor-not-allowed" title="iteration 4">
          {loopsIcon}Loops <span className="ml-auto num text-[10px] text-ink-500">I4</span>
        </span>
      </nav>
      <div className="slabel flex justify-between mx-[11px] mt-[22px] mb-[7px]">
        <span>Projects</span>
        <span className="text-[10px]">~/pm</span>
      </div>
      <div className="grid gap-[2px]">
        <button
          onClick={() => onProject("all")}
          className={`proj ${project === "all" ? "on" : ""}`}
        >
          <span className="truncate">All projects</span>
          <span className="num text-micro text-ink-500">{total}</span>
        </button>
        {projects.map((p) => (
          <button
            key={p.key}
            onClick={() => onProject(p.key)}
            className={`proj ${project === p.key ? "on" : ""}`}
          >
            <span className="truncate">{p.key}</span>
            <span className="num text-micro text-ink-500">
              {p.prefix} {p.issues}
            </span>
          </button>
        ))}
      </div>
      <div className="mt-auto pt-4 border-t border-ink-700 mx-1 text-ink-500 text-label leading-relaxed">
        <div className="slabel mb-1">session</div>
        no auth · loopback only
        <br />
        <span className="num text-micro">cadence.localhost:18000</span>
      </div>
    </aside>
  );
}
