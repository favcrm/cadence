import { useContext, useEffect, useState } from "react";
import { api } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import Button from "../../ui/Button";
import { WriteGate } from "../auth/WriteGate";
import type { Agent, AgentDetail } from "../../lib/types";
import AgentAvatar, { agentLook } from "./AgentAvatar";
import { RecoveryBlock } from "./AgentProfile";
import AgentTabBody, { type TabContext } from "./AgentTabs";
import {
  categoryLabel,
  durationText,
  hiredText,
  roleOf,
  runsOnText,
  type RolesState,
} from "./agentRoles";
import { AGENT_TABS, type AgentTab } from "./agentTabModel";
import {
  agentHoldsWorkButDead,
  agentLocation,
  conversationalOnlyCloud,
  lifecycleBadge,
  lifecycleOf,
} from "./agentView";
import "./agents.css";

/** CAD-1853: the daemon's truthful cloud-agent projection on an agent page.
 *  Remote ids are distinct from local ids; capabilities are the enforced
 *  ceiling (conversational only), never a claimed shell/git/effect grant. */
interface CloudAgentView {
  id: string;
  state: string;
  connected: boolean;
  principal?: { kind: string; label: string };
  remote_agent_id: string;
  runtime: string;
  model?: string | null;
  provider_session_id?: string | null;
  capabilities?: Record<string, boolean>;
  thread?: { id: string; link?: string | null } | null;
  sessions?: { remote_session_id: string; conversation_id: string; thread?: { id: string; link?: string | null } }[];
  operations?: { operation_id: string; status: string; reply?: { text: string } | null }[];
}

const TONE: Record<string, string> = {
  fail: "var(--color-fail)",
  warn: "var(--color-warn)",
  info: "var(--color-info)",
  ok: "var(--color-ok)",
  ink: "var(--color-ink-500)",
};

/** The full agent page at `agents/<alias>`: hero, stats, tabs. It replaces
 *  the old drawer and carries every banner and guard the drawer had. */
export default function AgentPage({
  alias,
  observed,
  roles,
  tab,
  onTab,
  onBack,
  onOpenIssue,
}: {
  alias: string;
  observed?: Agent;
  roles: RolesState;
  tab: AgentTab;
  onTab: (tab: AgentTab) => void;
  onBack: () => void;
  onOpenIssue: (id: string) => void;
}) {
  const writeBlock = useContext(WriteGate);
  const t = useTranslate();
  const [detail, setDetail] = useState<AgentDetail | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);

  useEffect(() => {
    // A reload after a save keeps what is on screen; only a new alias resets.
    if (retry === 0) setDetail(null);
    setErr(null);
    let live = true;
    api
      .agent(alias)
      .then((d) => live && setDetail(d))
      .catch((e) => live && setErr(String(e.message ?? e)));
    return () => {
      live = false;
    };
  }, [alias, retry]);

  // Esc leaves the page, unless it is closing something inside it.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      const target = event.target;
      if (
        target instanceof HTMLElement &&
        (target.closest("input, textarea, select, dialog, [contenteditable]") !== null)
      )
        return;
      onBack();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onBack]);

  const a = detail?.agent;
  // A collection observation keeps evidence available during loading/failure.
  // Detail carries provider evidence, but not selection provenance or grouping.
  const profile: Agent | undefined = a
    ? {
        group: "",
        group_root: false,
        parked: 0,
        ...observed,
        ...a,
        tasks: observed?.tasks ?? [],
        running: detail.running.length,
        queued: detail.queued,
        unknown: detail.unknown,
        fenced: detail.fenced,
        on: detail.on ?? observed?.on ?? [],
      }
    : observed;
  const isCloud = conversationalOnlyCloud(profile);
  const fenced = detail?.fenced ?? observed?.fenced;
  const recovery = detail?.recovery ?? observed?.recovery;
  const resume = detail?.resume ?? observed?.resume;
  const lifecycle = profile ? lifecycleOf(profile) : "normal";
  const deadHolding = profile ? agentHoldsWorkButDead(profile) : false;
  const params = (a?.params ?? observed?.params) as Record<string, unknown> | undefined;
  const role = profile
    ? roleOf({ params: params ?? profile.params, catalogue: observed?.catalogue }, roles.data)
    : null;
  const hosted = roles.data?.install.hosted ?? false;
  const badge = profile ? lifecycleBadge(profile, t) : null;
  const where = isCloud
    ? t("a cloud runtime")
    : profile && agentLocation(profile) === "cloud"
      ? t("a cloud sandbox")
      : t(runsOnText(hosted));
  const categoryText = role ? `${role.title} · ${t(categoryLabel(roles.data, role.category))}` : null;
  const figures = detail?.stats ?? null;
  const cloud =
    (a as unknown as { cloud_agents?: CloudAgentView[] } | undefined)?.cloud_agents ?? [];
  const count = (n: number | null | undefined) => (n === null || n === undefined ? "—" : String(n));
  const stats: [string, string][] =
    role?.kind === "apps"
      ? [
          ["Reports", count(figures?.reports)],
          ["Drafts sent", count(figures?.drafts_sent)],
          ["Avg. turn", durationText(figures?.avg_turn_secs)],
          ["Hired", hiredText(figures?.hired_at)],
        ]
      : [
          ["Tickets done", count(figures?.tasks_done)],
          ["PRs merged", count(figures?.prs_merged)],
          ["Avg. turn", durationText(figures?.avg_turn_secs)],
          ["Hired", hiredText(figures?.hired_at)],
        ];

  const ctx: TabContext = {
    alias,
    profile,
    observed,
    detail,
    role,
    hosted,
    resume,
    fenced,
    lifecycle,
    deadHolding,
    onOpenIssue,
    onTab,
    onSaved: () => setRetry((n) => n + 1),
    detailFailed: err !== null,
  };

  return (
    <main className="agents-page px-4 lg:px-8 pt-6 pb-9 w-full">
      <button type="button" className="rc-back" onClick={onBack}>
        ← {t("Agents")}
      </button>
      <header className="ap-hero">
        <AgentAvatar
          slug={agentLook(alias, params)}
          look={typeof params?.look === "string" ? params.look : undefined}
          still={lifecycle !== "active"}
          active={lifecycle === "active"}
          size={72}
        />
        <div className="min-w-0">
          <h1>{alias}</h1>
          <div className="ap-sub">
            <span>
              {categoryText ??
                `${a?.role ?? observed?.role ?? t("Agent")} · ${a?.provider ?? observed?.provider ?? t("Loading provider")}${profile ? ` / ${profile.endpoint_kind}` : ""}`}
            </span>
            {badge && (
              <>
                <span aria-hidden="true">·</span>
                <span className="rc-dot" style={{ "--c": TONE[badge.tone] } as React.CSSProperties} />
                <span>{badge.label}</span>
              </>
            )}
            <span aria-hidden="true">·</span>
            <span>{t("runs on {where}").replace("{where}", () => where)}</span>
          </div>
          {observed && (
            <p className="text-micro text-ink-500 mt-1">
              {t("Group:")} {observed.group_root ? t("root") : observed.group}
              {observed.team_role ? ` · ${t("Team role:")} ${observed.team_role}` : ""}
            </p>
          )}
          {role && <p className="ap-job">{role.job}</p>}
        </div>
        <div className="ap-acts">
          {/* Threads resolves the alias: the existing You ↔ agent conversation
              opens; otherwise `?dm=` starts a local draft — nothing is sent
              and no server row is created until the operator writes. */}
          <Button
            variant="primary"
            href={`/threads?dm=${encodeURIComponent(alias)}`}
            disabled={writeBlock !== null}
            title={writeBlock ?? t("Open the direct conversation with this agent")}
          >
            {t("Message")}
          </Button>
          {!isCloud && (
            <>
              <Button onClick={() => onTab("work")}>{t("Assign work")}</Button>
              <Button variant="ghost" size="sm" disabled aria-label={t("More")} title={t("No more actions yet")}>
                ⋯
              </Button>
            </>
          )}
        </div>
      </header>

      {cloud.length > 0 && (
        <section className="ap-cloud" aria-label={t("Cloud runtime")}>
          {cloud.map((c) => (
            <div key={c.id} className="ap-cloud-card">
              <div className="ap-sub">
                <strong>{t("Cloud runtime")}</strong>
                <span aria-hidden="true">·</span>
                <span>{c.runtime}</span>
                <span aria-hidden="true">·</span>
                <span>{c.state}</span>
                {c.principal && (
                  <>
                    <span aria-hidden="true">·</span>
                    <span>
                      {c.principal.kind === "agent"
                        ? `${t("enrolled peer")} ${c.principal.label}`
                        : t("operator connection")}
                    </span>
                  </>
                )}
              </div>
              <p className="text-micro text-ink-500">
                {t("Model:")} {c.model ?? t("gateway label pending")} · {t("Remote agent:")} {c.remote_agent_id}
              </p>
              <p className="text-micro text-ink-500">
                {t("Capabilities:")} {t("conversation")}
                {", "}
                {t("no tools, MCP, skills, approvals or shell")}
              </p>
              {(c.sessions ?? []).length > 1 && (
                <ul className="text-micro text-ink-500">
                  {(c.sessions ?? []).map((s) => (
                    <li key={s.conversation_id}>
                      {s.thread?.link ? (
                        <a href={s.thread.link}>{s.thread.id}</a>
                      ) : (
                        s.thread?.id ?? s.conversation_id
                      )}
                      {" · "}
                      {s.remote_session_id}
                    </li>
                  ))}
                </ul>
              )}
            </div>
          ))}
        </section>
      )}

      {!detail && !err && (
        <p role="status" className="text-label text-ink-500 mb-3">
          {t("Loading agent details…")}
        </p>
      )}
      {err && (
        <div className="space-y-2 mb-3">
          <p role="alert" className="text-label text-fail">
            {t("Could not load agent details:")} {err}
          </p>
          <Button onClick={() => setRetry((r) => r + 1)}>{t("Retry details")}</Button>
        </div>
      )}
      {deadHolding && (
        <div className="agents-banner tone-fail mb-3" role="status">
          {t("Agent is dead but still holds work — release the claim or reassign before it stalls the ticket.")}
        </div>
      )}
      {lifecycle === "stale-inbox" && profile && (
        <div className="agents-banner tone-info mb-3" role="status">
          {t("Mailbox only — {count} unread, no live endpoint. Drain or archive.").replace("{count}", String(profile.queued))}
        </div>
      )}
      {lifecycle === "idle-holding" && (
        <div className="agents-banner tone-warn mb-3" role="status">
          {t("Not running but still holds a claim — resume it or release the work.")}
        </div>
      )}
      {fenced && (
        <section className="mb-3">
          <h3 className="text-cardtitle font-semibold text-fail">{t("Recovery required")}</h3>
          <p className="text-label text-ink-400 mt-1">
            {t("An operator must inspect the outcome before reconciling.")}
          </p>
          <RecoveryBlock
            text={
              recovery ??
              t("Recovery guidance unavailable. Refresh agent details before taking action.")
            }
          />
        </section>
      )}

      {!isCloud && (
        <>
          <div className="ap-stats">
            {stats.map(([label, value]) => (
              <div
                key={label}
                className="ap-stat"
                title={value === "—" ? t("Not recorded for this agent") : undefined}
              >
                <b className="num">{value}</b>
                <small>{t(label)}</small>
              </div>
            ))}
          </div>
          <nav className="ap-tabs" role="tablist" aria-label={t("Agent sections")}>
            {AGENT_TABS.map((entry) => (
              <button
                key={entry.id}
                type="button"
                role="tab"
                id={`ap-tab-${entry.id}`}
                aria-selected={tab === entry.id}
                aria-controls="ap-panel"
                className="ap-tab"
                onClick={() => onTab(entry.id)}
              >
                {t(entry.label)}
              </button>
            ))}
          </nav>
          <div id="ap-panel" role="tabpanel" aria-labelledby={`ap-tab-${tab}`}>
            <AgentTabBody key={`${alias}:${tab}:${detail ? "d" : "n"}`} tab={tab} c={ctx} />
          </div>
        </>
      )}
    </main>
  );
}
