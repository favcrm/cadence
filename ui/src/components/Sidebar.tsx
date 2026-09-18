import type { Project } from "../types";

interface Props {
  tab: string;
  onTab: (tab: "board" | "plan" | "agents") => void;
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
const agentsIcon = (
  <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4">
    <rect x="2" y="3" width="12" height="8.5" rx="1.2" />
    <path d="M5.5 14h5" />
  </svg>
);

export default function Sidebar({ tab, onTab, project, onProject, projects, total }: Props) {
  return (
    <aside className="hidden lg:flex sticky top-0 h-screen flex-col border-r border-ink-700 bg-ink-875 px-[14px] pt-[22px] pb-4 overflow-y-auto">
      <div className="px-[7px] pb-[23px]">
        <span className="inline-flex items-center gap-2 text-ink-100 text-[17px] font-semibold tracking-[-.035em]">
          <svg width="25" height="25" viewBox="0 0 25 25" fill="none">
            <rect x=".5" y=".5" width="24" height="24" rx="5" stroke="#2e3136" />
            <path
              d="M6 16.5V8.5M10.3 16.5v-5M14.6 16.5V6.5M18.9 16.5v-3.4"
              stroke="#e7e9ec"
              strokeWidth="1.6"
              strokeLinecap="round"
            />
          </svg>
          <span>
            cadence<span className="text-ink-400 font-normal"> board</span>
          </span>
        </span>
      </div>
      <nav className="grid gap-[3px]">
        <button
          onClick={() => onTab("board")}
          className="navlink"
          aria-current={tab === "board" ? "page" : undefined}
        >
          {boardIcon}Board
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
