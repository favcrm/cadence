import { MASTER } from "../home/master";
import { useTranslate } from "../../lib/locale";
import type { Agent } from "../../lib/types";
import AgentAvatar, { agentLook } from "./AgentAvatar";
import { activeTasks } from "./AgentBlocks";
import { agentStatus, lifecycleBadge, lifecycleOf, type LifecycleTone } from "./agentView";

const TONE_COLOR: Record<LifecycleTone, string> = {
  fail: "var(--color-fail)",
  warn: "var(--color-warn)",
  info: "var(--color-info)",
  ok: "var(--color-ok)",
  ink: "var(--color-ink-500)",
};

/** The one-line state and its dot: "Working on CAD-1744", "Idle",
 *  "Fenced — holding work". Fleet states fold into this line. */
export function seatStatus(a: Agent, t: (english: string) => string): { text: string; tone: LifecycleTone } {
  const lifecycle = lifecycleOf(a);
  const badge = lifecycleBadge(a, t);
  const first = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);
  if (lifecycle === "dead-holding") {
    return { text: `${a.fenced ? t("Fenced") : t("Dead")} — ${t("holding work")}`, tone: "fail" };
  }
  if (lifecycle === "idle-holding") return { text: `${first(t(agentStatus(a)))} — ${t("holding claim")}`, tone: "warn" };
  if (lifecycle === "stale-inbox") {
    return { text: t("Mailbox — {count} unread").replace("{count}", String(a.queued)), tone: "info" };
  }
  if (lifecycle === "active") {
    const issue = activeTasks(a)[0]?.issue;
    return {
      text: issue ? t("Working on {issue}").replace("{issue}", issue) : t("Working"),
      tone: "ok",
    };
  }
  // The master at rest reads "Ready" (mockup), like a seat that can take work.
  if (a.alias === MASTER && badge.label === t("idle")) return { text: t("Ready"), tone: "ok" };
  // A plain Idle is grey like the mockup; the shared badge keeps its green.
  if (badge.label === t("idle")) return { text: first(badge.label), tone: "ink" };
  return { text: first(badge.label), tone: badge.tone };
}

/** The mockup's compact seat card: avatar, name, "role · runs on", a
 *  divider, then a status dot with one line. The whole card opens the
 *  agent's page; at most one small badge (the queue) rides the status row. */
export default function SeatCard({
  agent: a,
  roleLine,
  isNew,
  onOpen,
}: {
  agent: Agent;
  roleLine: string;
  isNew?: boolean;
  onOpen: () => void;
}) {
  const t = useTranslate();
  const status = seatStatus(a, t);
  const lifecycle = lifecycleOf(a);
  const badge = a.queued > 0 && lifecycle !== "stale-inbox" ? t("{count} queued").replace("{count}", String(a.queued)) : null;
  return (
    <button
      type="button"
      className={`rc-seat clickable${isNew ? " new" : ""}`}
      aria-label={t("Open agent {alias}").replace("{alias}", () => a.alias)}
      onClick={onOpen}
    >
      <AgentAvatar
        slug={agentLook(a.alias, { look: a.look ?? a.params?.look })}
        active={lifecycle === "active"}
        still={lifecycle !== "active"}
        size={44}
      />
      <span className="min-w-0">
        <span className="t">{a.alias}</span>
        <span className="s">{roleLine}</span>
      </span>
      <span className="st">
        <span className="rc-dot" style={{ "--c": TONE_COLOR[status.tone] } as React.CSSProperties} />
        <span className="min-w-0 truncate">{status.text}</span>
        {badge && <span className="rc-badge ml-auto">{badge}</span>}
      </span>
    </button>
  );
}
