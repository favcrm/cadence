import type { KeyboardEvent, ReactNode } from "react";
import { useTranslate } from "../../lib/locale";
import type { Agent, HiredProfile } from "../../lib/types";
import { IconSearch } from "../../ui/icons";
import AgentAvatar, { agentLook } from "./AgentAvatar";
import { seatStatus } from "./SeatCard";
import { agentCategory, agentOrder, liveClaimState } from "./agentView";

/** The list's groups, in the order they read. Stopped and Mailboxes fold. */
const GROUPS: { id: string; label: string; folded?: boolean }[] = [
  { id: "attention", label: "Needs you" },
  { id: "working", label: "Working" },
  { id: "idle", label: "Idle" },
  { id: "unknown", label: "Unknown" },
  { id: "profile", label: "Not running" },
  { id: "stopped", label: "Stopped", folded: true },
  { id: "inbox", label: "Mailboxes", folded: true },
];

/** A profile sits with the agents its live claim matches; its card says the same. */
function profileGroup(p: HiredProfile): string {
  const live = liveClaimState(p);
  return live === "working" ? "working" : live === "blocked" ? "attention" : "profile";
}

function groupOf(a: Agent): string {
  const category = agentCategory(a);
  return category === "other" ? (a.state == null ? "unknown" : "idle") : category;
}

export default function AgentSidebar({
  agents,
  profiles,
  profileStatus,
  open,
  query,
  onQuery,
  onOpen,
  onOverview,
  onRecruit,
}: {
  agents: Agent[];
  profiles: HiredProfile[];
  /** A profile's one status line, as its card reads it. */
  profileStatus: (profile: HiredProfile) => string;
  open: string | null;
  query: string;
  onQuery: (query: string) => void;
  onOpen: (alias: string) => void;
  onOverview: () => void;
  onRecruit: () => void;
}) {
  const t = useTranslate();
  const row = (alias: string, face: ReactNode, status: string) => (
    <button
      key={alias}
      type="button"
      className="ag-side-row"
      aria-current={open === alias ? "page" : undefined}
      onClick={() => onOpen(alias)}
    >
      {face}
      <span className="min-w-0">
        <span className="t">{alias}</span>
        <span className="s" title={status}>
          {status}
        </span>
      </span>
    </button>
  );
  const groups = GROUPS.map((group) => {
    const rows =
      [
        ...agents
          .filter((a) => groupOf(a) === group.id)
          .sort((a, b) => agentOrder(a) - agentOrder(b) || a.alias.localeCompare(b.alias))
          .map((a) =>
            row(
              a.alias,
              <AgentAvatar
                slug={agentLook(a.alias, a.params as Record<string, unknown> | undefined)}
                size={28}
                still
              />,
              seatStatus(a, t).text,
            ),
          ),
        ...profiles
          .filter((p) => profileGroup(p) === group.id)
          .map((p) =>
            row(p.alias, <AgentAvatar slug="custom" look={p.look ?? undefined} size={28} still />, profileStatus(p)),
          ),
      ];
    return { ...group, rows };
  }).filter((group) => group.rows.length > 0);

  // Arrows move along the visible rows; Enter opens one.
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    const rows = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>(".ag-side-row")).filter(
      (el) => el.offsetParent !== null,
    );
    const at = rows.indexOf(document.activeElement as HTMLButtonElement);
    if (at < 0 || rows.length === 0) return;
    event.preventDefault();
    rows[(at + (event.key === "ArrowDown" ? 1 : -1) + rows.length) % rows.length].focus();
  };

  return (
    <aside className="ag-side" aria-label={t("Agents")}>
      <div className="ag-side-head">
        <strong>{t("Agents")}</strong>
        <button type="button" className="ag-side-add" aria-label={t("Recruit an agent")} title={t("Recruit")} onClick={onRecruit}>
          ＋
        </button>
      </div>
      <label className="ag-search ag-side-search">
        <IconSearch />
        <span className="sr-only">{t("Search agents")}</span>
        <input type="search" value={query} onChange={(e) => onQuery(e.target.value)} placeholder={t("Search agents")} />
      </label>
      <div className="ag-side-rows" onKeyDown={onKeyDown}>
        <button
          type="button"
          className="ag-side-row plain"
          aria-current={open === null ? "page" : undefined}
          onClick={onOverview}
        >
          <span className="t">{t("Overview")}</span>
        </button>
        {groups.map((group) =>
          group.folded ? (
            <details key={group.id} open={query.trim() !== "" || group.rows.some((r) => r.key === open)}>
              <summary className="ag-side-group">
                {t(group.label)}
                <span className="num">{group.rows.length}</span>
              </summary>
              {group.rows}
            </details>
          ) : (
            <div key={group.id}>
              <div className="ag-side-group">
                {t(group.label)}
                <span className="num">{group.rows.length}</span>
              </div>
              {group.rows}
            </div>
          ),
        )}
        {groups.length === 0 && query.trim() !== "" && (
          <p className="ag-side-none" role="status">
            {t("No agents match “{query}”.").replace("{query}", () => query)}{" "}
            <button type="button" className="lnk" onClick={() => onQuery("")}>
              {t("Clear search")}
            </button>
          </p>
        )}
      </div>
    </aside>
  );
}
