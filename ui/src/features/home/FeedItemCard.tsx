import { useEffect, useState } from "react";
import { useTranslate } from "../../lib/locale";
import type { AgentFeedItem } from "../../lib/types";
import AgentAvatar, { agentLook } from "../agents/AgentAvatar";
import {
  feedToneForState,
  formatFeedAge,
  formatFeedClock,
  isLiveFeedState,
} from "./feedRailModel";

/**
 * Isolated 1s clock/age leaf so ticking live timers never re-render the parent
 * Feed card or interrupt hover/focus transitions.
 */
function LiveTickMeta({
  updatedAt,
  startedAt,
  live,
}: {
  updatedAt: string;
  startedAt?: string | null;
  live: boolean;
}) {
  const [nowMs, setNowMs] = useState(() => Date.now());

  useEffect(() => {
    const timer = window.setInterval(() => setNowMs(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, []);

  const updatedMs = Date.parse(updatedAt);
  const ageText = Number.isFinite(updatedMs)
    ? formatFeedAge(Math.max(0, (nowMs - updatedMs) / 1000))
    : null;
  const startedMs = startedAt ? Date.parse(startedAt) : NaN;
  const clockText =
    live && Number.isFinite(startedMs)
      ? formatFeedClock(Math.max(0, (nowMs - startedMs) / 1000))
      : null;

  return (
    <>
      {clockText ? <span className="num">{clockText}</span> : null}
      {ageText ? <span className="num text-ink-500">· {ageText}</span> : null}
    </>
  );
}

/**
 * Revamped compact Home right-panel Feed card (`NeedsRail -> Feed`):
 * - Clean identity (28px SVG AgentAvatar + alias + role + live state)
 * - Stable ticket & task title + 1-line live `latest_action`
 * - Progressive disclosure (`Command ▾` / `Steps ▾`) collapsed by default
 * - Zero-layout-shift hover/focus bar (`Copy CLI`, `Ask`, `Details ›`)
 */
export function FeedItemCard({
  item,
  justUpdated = false,
  unread = false,
  onAsk,
  onOpenDetails,
}: {
  item: AgentFeedItem;
  justUpdated?: boolean;
  unread?: boolean;
  onAsk?: (item: AgentFeedItem) => void;
  onOpenDetails?: (item: AgentFeedItem, opener: HTMLElement) => void;
}) {
  const t = useTranslate();
  const [expanded, setExpanded] = useState(false);
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">("idle");

  const live = isLiveFeedState(item.state);
  const tone = feedToneForState(item.state);
  const dotCls = live ? "live" : tone;
  const hasAdvanced = Boolean(
    item.operator_action || (item.steps && item.steps.length > 0) || item.runner_label || item.head_sha,
  );
  const expandLabel = item.operator_action
    ? t(expanded ? "Hide command ▴" : "Command ▾")
    : t(expanded ? "Hide steps ▴" : "Steps ▾");

  const handleCopyCli = async () => {
    const cmd = item.operator_action?.command;
    if (!cmd) return;
    if (!navigator.clipboard?.writeText) {
      setCopyState("failed");
      window.setTimeout(() => setCopyState("idle"), 1600);
      return;
    }
    try {
      await navigator.clipboard.writeText(cmd);
      setCopyState("copied");
    } catch {
      setCopyState("failed");
    }
    window.setTimeout(() => setCopyState("idle"), 1600);
  };

  return (
    <li
      className={`rc2${expanded ? " is-expanded" : ""}${justUpdated ? " just-updated" : ""}`}
      data-feed-item={item.id}
      data-feed-state={item.state}
    >
      <div className="rc2-top">
        <div className="rc2-actor">
          <AgentAvatar
            slug={agentLook(item.alias, item.look ? { look: item.look } : undefined)}
            look={item.look ?? undefined}
            size={28}
            active={live}
            still={!live}
          />
          <div className="rc2-actor-meta">
            <div className="rc2-alias-line">
              {unread ? <span className="unread-dot" aria-hidden="true" /> : null}
              <span className="rc2-alias">{item.alias}</span>
            </div>
            <span className="rc2-role">{item.role}</span>
          </div>
        </div>

        <div className="rc2-state">
          <span className={`sdot ${dotCls}`} aria-hidden="true" />
          <span className={`rc2-state-label ${tone}`}>{t(item.state_label)}</span>
          <LiveTickMeta updatedAt={item.updated_at} startedAt={item.started_at} live={live} />
        </div>
      </div>

      <div className="rc2-work">
        <div className="rc2-title-line">
          {item.ticket_id ? <span className="rc2-ticket">{item.ticket_id}</span> : null}
          <h3 className="rc2-title">{item.task_title}</h3>
        </div>
        <div className="rc2-action-line">
          <span className="rc2-verb">{item.action_verb}</span>
          <span className="rc2-action-text">{item.latest_action}</span>
        </div>
      </div>

      {hasAdvanced ? (
        <div className="adv-fold" aria-hidden={!expanded}>
          <div className="adv-fold-in">
            {item.operator_action ? (
              <div className="adv-box op-warn">
                <div className="adv-head">
                  <strong>{t("Operator-only terminal command")}</strong>
                  {item.operator_action.gates?.length ? (
                    <span className="mono">
                      {item.operator_action.gates.filter((g) => g.ok).length}/
                      {item.operator_action.gates.length} {t("gates")}
                    </span>
                  ) : null}
                </div>
                <pre className="cmd-pre">{item.operator_action.command}</pre>
                <div className="text-[10.5px] text-ink-400">
                  {t("Run locally in your terminal — never auto-approved by agents.")}
                </div>
              </div>
            ) : (
              <div className="adv-box">
                {(item.runner_label || item.head_sha) ? (
                  <div className="adv-head">
                    <span className="mono">{item.runner_label ?? ""}</span>
                    {item.head_sha ? (
                      <span className="mono">{item.head_sha.slice(0, 12)}</span>
                    ) : null}
                  </div>
                ) : null}
                {item.steps && item.steps.length > 0 ? (
                  <div className="step-row">
                    {item.steps.map((step, idx) => (
                      <span key={`${step.label}:${idx}`} className="step-chip" data-s={step.status}>
                        {step.label}
                      </span>
                    ))}
                  </div>
                ) : null}
              </div>
            )}
          </div>
        </div>
      ) : null}

      <div className="rc2-foot">
        <div className="rc2-rest">
          <span className="rc2-rest-left">
            <span className="mono rc2-ref">{item.pr_ref ?? item.runner_label ?? ""}</span>
            {item.operator_action ? (
              <span className="op-hint-pill">· {t("Operator CLI")}</span>
            ) : null}
          </span>
          <span className="rc2-rest-right">{t("Details ›")}</span>
        </div>

        <div className="rc2-hover-bar">
          <div className="hover-btn-group">
            {item.operator_action ? (
              <button
                type="button"
                className="act-btn warn"
                onClick={() => void handleCopyCli()}
              >
                {copyState === "copied" ? t("Copied ✓") : copyState === "failed" ? t("Copy failed") : t("Copy CLI")}
              </button>
            ) : null}
            {hasAdvanced ? (
              <button
                type="button"
                className="act-btn"
                aria-expanded={expanded}
                onClick={() => setExpanded((prev) => !prev)}
              >
                {expandLabel}
              </button>
            ) : null}
            {onAsk && !item.operator_action ? (
              <button
                type="button"
                className="act-btn"
                onClick={() => onAsk(item)}
              >
                {t("Ask")}
              </button>
            ) : null}
          </div>
          {onOpenDetails ? (
            <button
              type="button"
              className="act-btn ghost"
              onClick={(event) => onOpenDetails(item, event.currentTarget)}
            >
              {t("Details ›")}
            </button>
          ) : null}
        </div>
      </div>
    </li>
  );
}
