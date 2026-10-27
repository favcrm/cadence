import { fmtTime } from "../../lib/fmt";
import { provenanceDetail } from "./modelProvenance";
import { useTranslate } from "../../lib/locale";
import type { Agent, UsageLimit } from "../../lib/types";

/** The model / effort / usage and recovery blocks the agent page's Overview
 *  and Settings tabs share (moved verbatim from the retired AgentDrawer). */

type Translate = (english: string) => string;
const identity: Translate = (english) => english;

/// A fence's recovery path — the daemon's own error text already names
/// `agent unfence` / `message reconcile`; render it verbatim as code.
export function RecoveryBlock({ text }: { text: string }) {
  return (
    <pre className="mt-1.5 whitespace-pre-wrap rounded border border-fail/30 bg-fail/5 px-2.5 py-2 text-micro text-fail/90 font-mono">
      {text}
    </pre>
  );
}

function profileModel(a: Agent, t: Translate = identity): {
  value: string;
  note: string;
  mismatch?: string;
  provenance?: string;
} {
  const reported = a.model_reported ?? a.model ?? null;
  const configured = a.model_configured ?? null;
  const provenanceText = provenanceDetail(
    a.model_selection,
    a.model_lookup_role,
    t,
  );
  if (
    a.model_selection === null &&
    (a.provider === "devin" || a.provider === "inbox")
  ) {
    return {
      value: reported ?? t("unsupported"),
      note: t("model selection unsupported"),
      provenance: provenanceText || undefined,
    };
  }
  if (reported) {
    return {
      value: reported,
      note: t("reported"),
      mismatch:
        configured && configured !== reported
          ? t("configured {model}").replace("{model}", () => configured)
          : undefined,
      provenance: provenanceText || undefined,
    };
  }
  if (configured) {
    return {
      value: configured,
      note: t("configured"),
      provenance: provenanceText || undefined,
    };
  }
  if (a.model_source === "provider default") {
    return {
      value: t("provider default"),
      note: t("reported model unknown"),
      provenance: provenanceText || undefined,
    };
  }
  return {
    value: t("unknown"),
    note: t("no provider evidence"),
    provenance: provenanceText || undefined,
  };
}

function profileEffort(a: Agent, t: Translate = identity): { value: string; note: string } {
  if (a.effort_applicable === false || a.effort_source === "not_applicable") {
    return { value: "n/a", note: t("provider does not report effort") };
  }
  const effective = a.effort_reported ?? null;
  if (effective) return { value: effective, note: t("confirmed") };
  if (a.effort) return { value: a.effort, note: t("configured") };
  return { value: t("unknown"), note: t("no provider evidence") };
}

function usageFor(a: Agent): UsageLimit | null {
  return a.quota ?? a.usage_limit ?? null;
}

function usageText(a: Agent, t: Translate = identity): { value: string; note: string; tone: string } {
  const quota = usageFor(a);
  if (!quota) {
    return {
      value: t("unavailable"),
      note: t("provider quota telemetry is not integrated"),
      tone: "text-ink-500",
    };
  }
  const state = quota.state ?? "unknown";
  if (state === "error") {
    return {
      value: t("error"),
      note: quota.message ?? quota.reason ?? t("quota query failed"),
      tone: "text-fail",
    };
  }
  if (state === "stale") {
    return {
      value: t("stale"),
      note: [
        quota.observed_at
          ? t("last update {time}").replace("{time}", () => fmtTime(quota.observed_at!))
          : t("last update unknown"),
        quota.window,
        quota.source ? t("source {source}").replace("{source}", () => String(quota.source)) : null,
      ]
        .filter(Boolean)
        .join(" · "),
      tone: "text-warn",
    };
  }
  if (state === "blocked") {
    return {
      value: t("blocked"),
      note: quota.reason ?? t("provider reported a quota block"),
      tone: "text-fail",
    };
  }
  if (state !== "available") {
    return {
      value: t("unknown"),
      note: quota.reason ?? t("allowance not reported"),
      tone: "text-ink-500",
    };
  }
  // Null means unavailable; zero is a real value and must remain visible.
  const unit = quota.unit ? ` ${quota.unit}` : "";
  const remaining =
    quota.remaining != null ? `${quota.remaining}${unit} ${t("remaining")}` : null;
  const used = quota.used != null ? `${quota.used}${unit} ${t("used")}` : null;
  const limit = quota.limit != null ? `${quota.limit}${unit} ${t("limit")}` : null;
  const usedAgainstLimit =
    quota.used != null && quota.limit != null
      ? `${quota.used}/${quota.limit}${unit} ${t("used")}`
      : null;
  const percent =
    quota.used_percent != null ? `${quota.used_percent}% ${t("used")}` : null;
  const value =
    remaining ?? usedAgainstLimit ?? used ?? percent ?? limit ?? t("available");
  const window = quota.window ? `${quota.window}` : null;
  const reset = quota.reset_at ? t("reset {time}").replace("{time}", () => fmtTime(quota.reset_at!)) : null;
  const pool = quota.pool
    ? `${t("shared")}${quota.pool.label ? ` · ${quota.pool.label}` : ""}${
        quota.pool.count != null ? ` · ${t("{count} agents").replace("{count}", String(quota.pool.count))}` : ""
      }`
    : null;
  const poolAgents = quota.pool?.agents?.length
    ? `${t("agents")} ${quota.pool.agents.join(", ")}`
    : null;
  const lastUpdate = quota.updated_at ?? quota.observed_at;
  return {
    value,
    note:
      [
        window,
        reset,
        pool,
        poolAgents,
        lastUpdate ? t("last update {time}").replace("{time}", () => fmtTime(lastUpdate)) : null,
        quota.source ? t("source {source}").replace("{source}", () => String(quota.source)) : null,
      ]
        .filter(Boolean)
        .join(" · ") || t("reported"),
    tone: "text-ink-300",
  };
}

export function ProfileBlock({ agent }: { agent: Agent }) {
  const t = useTranslate();
  const model = profileModel(agent, t);
  const effort = profileEffort(agent, t);
  const usage = usageText(agent, t);
  return (
    <div className="space-y-1 min-w-0 agents-profile">
      <div
        className="num text-ink-200 truncate"
        title={`${model.value} · ${model.note}`}
      >
        {model.value}
      </div>
      <div className="num text-micro text-ink-500">
        {t("model")} {model.note}
        {model.mismatch ? ` · ${model.mismatch}` : ""}
      </div>
      {model.provenance && (
        <div className="num text-micro text-ink-500">{model.provenance}</div>
      )}
      <div className="num text-ink-300">{t("effort")} {effort.value}</div>
      <div className="num text-micro text-ink-500">{effort.note}</div>
      <div className={`num text-ink-300 ${usage.tone}`}>{usage.value}</div>
      <div className="num text-micro text-ink-500" title={usage.note}>
        {usage.note}
      </div>
    </div>
  );
}

