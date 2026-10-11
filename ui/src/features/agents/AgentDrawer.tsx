import { useEffect, useRef, useState } from "react";
import { api } from "../../lib/api";
import { fmtTime } from "../../lib/fmt";
import { provenanceDetail } from "./modelProvenance";
import { IconClose } from "../../ui/icons";
import Button from "../../ui/Button";
import AgentAvatar, { lookFor } from "./AgentAvatar";
import { AgentActivity, AgentQueue, WorkBlock } from "./AgentBlocks";
import { agentHoldsWorkButDead, lifecycleOf } from "./agentView";
import type { Agent, AgentDetail, UsageLimit } from "../../lib/types";

/// A fence's recovery path — the daemon's own error text already names
/// `agent unfence` / `message reconcile`; render it verbatim as code.
function RecoveryBlock({ text }: { text: string }) {
  return (
    <pre className="mt-1.5 whitespace-pre-wrap rounded border border-fail/30 bg-fail/5 px-2.5 py-2 text-micro text-fail/90 font-mono">
      {text}
    </pre>
  );
}

function profileModel(a: Agent): {
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
  );
  if (
    a.model_selection === null &&
    (a.provider === "devin" || a.provider === "inbox")
  ) {
    return {
      value: reported ?? "unsupported",
      note: "model selection unsupported",
      provenance: provenanceText || undefined,
    };
  }
  if (reported) {
    return {
      value: reported,
      note: "reported",
      mismatch:
        configured && configured !== reported
          ? `configured ${configured}`
          : undefined,
      provenance: provenanceText || undefined,
    };
  }
  if (configured) {
    return {
      value: configured,
      note: "configured",
      provenance: provenanceText || undefined,
    };
  }
  if (a.model_source === "provider default") {
    return {
      value: "provider default",
      note: "reported model unknown",
      provenance: provenanceText || undefined,
    };
  }
  return {
    value: "unknown",
    note: "no provider evidence",
    provenance: provenanceText || undefined,
  };
}

function profileEffort(a: Agent): { value: string; note: string } {
  if (a.effort_applicable === false || a.effort_source === "not_applicable") {
    return { value: "n/a", note: "provider does not report effort" };
  }
  const effective = a.effort_reported ?? null;
  if (effective) return { value: effective, note: "confirmed" };
  if (a.effort) return { value: a.effort, note: "configured" };
  return { value: "unknown", note: "no provider evidence" };
}

function usageFor(a: Agent): UsageLimit | null {
  return a.quota ?? a.usage_limit ?? null;
}

function usageText(a: Agent): { value: string; note: string; tone: string } {
  const quota = usageFor(a);
  if (!quota) {
    return {
      value: "unavailable",
      note: "provider quota telemetry is not integrated",
      tone: "text-ink-500",
    };
  }
  const state = quota.state ?? "unknown";
  if (state === "error") {
    return {
      value: "error",
      note: quota.message ?? quota.reason ?? "quota query failed",
      tone: "text-fail",
    };
  }
  if (state === "stale") {
    return {
      value: "stale",
      note: [
        quota.observed_at
          ? `last update ${fmtTime(quota.observed_at)}`
          : "last update unknown",
        quota.window,
        quota.source ? `source ${quota.source}` : null,
      ]
        .filter(Boolean)
        .join(" · "),
      tone: "text-warn",
    };
  }
  if (state === "blocked") {
    return {
      value: "blocked",
      note: quota.reason ?? "provider reported a quota block",
      tone: "text-fail",
    };
  }
  if (state !== "available") {
    return {
      value: "unknown",
      note: quota.reason ?? "allowance not reported",
      tone: "text-ink-500",
    };
  }
  // Null means unavailable; zero is a real value and must remain visible.
  const unit = quota.unit ? ` ${quota.unit}` : "";
  const remaining =
    quota.remaining != null ? `${quota.remaining}${unit} remaining` : null;
  const used = quota.used != null ? `${quota.used}${unit} used` : null;
  const limit = quota.limit != null ? `${quota.limit}${unit} limit` : null;
  const usedAgainstLimit =
    quota.used != null && quota.limit != null
      ? `${quota.used}/${quota.limit}${unit} used`
      : null;
  const percent =
    quota.used_percent != null ? `${quota.used_percent}% used` : null;
  const value =
    remaining ?? usedAgainstLimit ?? used ?? percent ?? limit ?? "available";
  const window = quota.window ? `${quota.window}` : null;
  const reset = quota.reset_at ? `reset ${fmtTime(quota.reset_at)}` : null;
  const pool = quota.pool
    ? `shared${quota.pool.label ? ` · ${quota.pool.label}` : ""}${
        quota.pool.count != null ? ` · ${quota.pool.count} agents` : ""
      }`
    : null;
  const poolAgents = quota.pool?.agents?.length
    ? `agents ${quota.pool.agents.join(", ")}`
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
        lastUpdate ? `last update ${fmtTime(lastUpdate)}` : null,
        quota.source ? `source ${quota.source}` : null,
      ]
        .filter(Boolean)
        .join(" · ") || "reported",
    tone: "text-ink-300",
  };
}

function ProfileBlock({ agent }: { agent: Agent }) {
  const model = profileModel(agent);
  const effort = profileEffort(agent);
  const usage = usageText(agent);
  return (
    <div className="space-y-1 min-w-0 agents-profile">
      <div
        className="num text-ink-200 truncate"
        title={`${model.value} · ${model.note}`}
      >
        {model.value}
      </div>
      <div className="num text-micro text-ink-500">
        model {model.note}
        {model.mismatch ? ` · ${model.mismatch}` : ""}
      </div>
      {model.provenance && (
        <div className="num text-micro text-ink-500">{model.provenance}</div>
      )}
      <div className="num text-ink-300">effort {effort.value}</div>
      <div className="num text-micro text-ink-500">{effort.note}</div>
      <div className={`num text-ink-300 ${usage.tone}`}>{usage.value}</div>
      <div className="num text-micro text-ink-500" title={usage.note}>
        {usage.note}
      </div>
    </div>
  );
}

export default function AgentDrawer({
  alias,
  observed,
  onClose,
  onOpenIssue,
}: {
  alias: string;
  observed?: Agent;
  onClose: () => void;
  onOpenIssue: (id: string) => void;
}) {
  const [detail, setDetail] = useState<AgentDetail | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);
  const dialogRef = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    setDetail(null);
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

  useEffect(() => {
    const dialog = dialogRef.current;
    const trigger = document.activeElement;
    const overflow = document.body.style.overflow;
    dialog?.showModal();
    document.body.style.overflow = "hidden";
    return () => {
      dialog?.close();
      document.body.style.overflow = overflow;
      if (trigger instanceof HTMLElement && trigger.isConnected)
        trigger.focus();
    };
  }, []);

  const a = detail?.agent;
  // A collection observation keeps evidence available during loading/failure.
  // Detail carries provider evidence, but not selection provenance or grouping.
  const profile: Agent | undefined = a
    ? {
        group: "",
        parked: 0,
        ...observed,
        ...a,
        tasks: observed?.tasks,
        running: detail.running.length,
        queued: detail.queued,
        unknown: detail.unknown,
        fenced: detail.fenced,
        on: detail.on ?? observed?.on ?? [],
      }
    : observed;
  const fenced = detail?.fenced ?? observed?.fenced;
  const recovery = detail?.recovery ?? observed?.recovery;
  const resume = detail?.resume ?? observed?.resume;
  const lifecycle = profile ? lifecycleOf(profile) : "normal";
  const deadHolding = profile ? agentHoldsWorkButDead(profile) : false;

  return (
    <dialog
      ref={dialogRef}
      className="agents-drawer bg-ink-875 border-l border-ink-700 text-ink-200"
      aria-labelledby="agent-detail-title"
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
      onClick={(event) => {
        if (event.target !== event.currentTarget) return;
        const rect = event.currentTarget.getBoundingClientRect();
        if (
          event.clientX < rect.left ||
          event.clientX > rect.right ||
          event.clientY < rect.top ||
          event.clientY > rect.bottom
        )
          onClose();
      }}
    >
      <header className="px-5 pt-4 pb-4 border-b border-ink-700 flex items-center gap-3 shrink-0">
        <AgentAvatar
          slug={lookFor(alias)}
          still={lifecycle !== "active"}
          active={lifecycle === "active"}
          size={52}
        />
        <div className="min-w-0 flex-1">
          <h2
            id="agent-detail-title"
            className="text-drawer font-semibold text-ink-100 leading-tight break-words"
          >
            {alias}
          </h2>
          <p className="text-label text-ink-400 mt-1">
            {a?.role ?? observed?.role ?? "Agent"} ·{" "}
            {a?.provider ?? observed?.provider ?? "Loading provider"}
            {profile && ` / ${profile.endpoint_kind}`}
          </p>
          {observed && (
            <p className="text-micro text-ink-500 mt-1">
              Group: {observed.group_root ? "root" : observed.group}
              {observed.team_role ? ` · Team role: ${observed.team_role}` : ""}
            </p>
          )}
        </div>
        <Button
          icon={<IconClose />}
          aria-label="Close agent details"
          className="agents-close"
          onClick={onClose}
        />
      </header>

      <div className="flex-1 overflow-y-auto px-5 py-5 space-y-6">
        {!detail && !err && (
          <p role="status" className="text-label text-ink-500">
            Loading agent details…
          </p>
        )}
        {err && (
          <div className="space-y-2">
            <p role="alert" className="text-label text-fail">
              Could not load agent details: {err}
            </p>
            <Button onClick={() => setRetry((r) => r + 1)}>
              Retry details
            </Button>
          </div>
        )}
        {deadHolding && (
          <div className="agents-banner tone-fail" role="status">
            Agent is dead but still holds work — release the claim or
            reassign before it stalls the ticket.
          </div>
        )}
        {lifecycle === "stale-inbox" && (
          <div className="agents-banner tone-info" role="status">
            Mailbox only — {profile!.queued} unread, no live endpoint. Drain
            or archive.
          </div>
        )}
        {lifecycle === "idle-holding" && (
          <div className="agents-banner tone-warn" role="status">
            Not running but still holds a claim — resume it or release the
            work.
          </div>
        )}
        {fenced && (
          <section>
            <h3 className="text-cardtitle font-semibold text-fail">
              Recovery required
            </h3>
            <p className="text-label text-ink-400 mt-1">
              An operator must inspect the outcome before reconciling.
            </p>
            <RecoveryBlock
              text={
                recovery ??
                "Recovery guidance unavailable. Refresh agent details before taking action."
              }
            />
          </section>
        )}
        {(observed || (detail && a)) && (
          <section>
            <h3 className="text-cardtitle font-semibold text-ink-100 mb-2">
              Current work
            </h3>
            {observed && (
              <>
                <WorkBlock agent={observed} onOpenIssue={onOpenIssue} />
                <AgentQueue agent={observed} />
                <div className="mt-2">
                  <AgentActivity agent={observed} />
                </div>
              </>
            )}
            {detail && a && (
              <>
                {(detail.tasks?.length ?? 0) > 0 && (
                  <div className="mt-3">
                    <div className="flex items-baseline gap-2 mb-2">
                      <h4 className="slabel">Tasks</h4>
                      <span className="kicker">assigned in flight</span>
                    </div>
                    <ul className="border border-ink-700 rounded-lg divide-y divide-ink-700/80 bg-ink-850">
                      {detail.tasks!.map((t) => (
                        <li
                          key={t}
                          className="flex items-center gap-2.5 px-3 py-2.5"
                        >
                          <span className="num text-label text-ink-200 break-words">
                            {t}
                          </span>
                        </li>
                      ))}
                    </ul>
                  </div>
                )}
                {(detail.on?.length ?? 0) > 0 && (
                  <div className="mt-3">
                    <div className="flex items-baseline gap-2 mb-2">
                      <h4 className="slabel">Issues</h4>
                      <span className="kicker">bound through the job</span>
                    </div>
                    <div className="flex flex-wrap gap-1.5">
                      {detail.on!.map((id) => (
                        <button
                          key={id}
                          className="lnk"
                          onClick={() => onOpenIssue(id)}
                        >
                          {id}
                        </button>
                      ))}
                    </div>
                  </div>
                )}
              </>
            )}
          </section>
        )}
        {detail && a && (
          <>
            <section>
              <div className="flex items-baseline gap-2 mb-2">
                <h3 className="text-cardtitle font-semibold text-ink-100">
                  Identity
                </h3>
              </div>
              <dl className="grid grid-cols-[7rem_minmax(0,1fr)] gap-y-1.5 text-label">
                {(
                  [
                    ["thread", a.thread_id],
                    ["session", a.session_id],
                    ["endpoint", a.endpoint],
                    ["pid", a.pid],
                    ["model", a.model],
                    ["generation", a.generation],
                    ["cwd", a.cwd],
                    ["sandbox", a.sandbox],
                  ] as [string, unknown][]
                )
                  .filter(([, v]) => v !== null && v !== undefined && v !== "")
                  .map(([k, v]) => (
                    <div key={k} className="contents">
                      <dt className="slabel">{k}</dt>
                      <dd
                        className="num text-ink-300 break-words"
                        title={String(v)}
                      >
                        {String(v)}
                      </dd>
                    </div>
                  ))}
                {Object.entries(a.params ?? {}).map(([k, v]) => (
                  <div key={k} className="contents">
                    <dt className="slabel" title={`param ${k}`}>
                      {k}
                    </dt>
                    <dd
                      className="num text-ink-300 break-words"
                      title={typeof v === "string" ? v : JSON.stringify(v)}
                    >
                      {typeof v === "string" ? v : JSON.stringify(v)}
                    </dd>
                  </div>
                ))}
              </dl>
            </section>

            {(detail.running.length > 0 || detail.queued > 0) && (
              <section>
                <div className="flex items-baseline gap-2 mb-2">
                  <h3 className="text-cardtitle font-semibold text-ink-100">
                    Inbox
                  </h3>
                  <span className="kicker">
                    {detail.running.length} running · {detail.queued} queued ·{" "}
                    {detail.unknown} unknown
                  </span>
                </div>
                <ul className="border border-ink-700 rounded-lg divide-y divide-ink-700/80 bg-ink-850">
                  {detail.running.map((m) => (
                    <li
                      key={m.id}
                      className="flex items-center gap-2.5 px-3 py-2.5"
                    >
                      <i className="w-1.5 h-1.5 rounded-full bg-info" />
                      <span className="num text-label text-ink-200 break-words">
                        {m.id}
                      </span>
                      {m.task && (
                        <span className="num text-micro text-ink-500">
                          task {m.task}
                        </span>
                      )}
                      {m.summary && (
                        <span
                          className="num text-micro text-ink-400 truncate"
                          title={m.summary}
                        >
                          {m.summary}
                        </span>
                      )}
                      {m.created && (
                        <span className="num text-micro text-ink-500 ml-auto">
                          {fmtTime(m.created)}
                        </span>
                      )}
                    </li>
                  ))}
                </ul>
              </section>
            )}

            <section>
              <div className="flex items-baseline gap-2 mb-2">
                <h3 className="text-cardtitle font-semibold text-ink-100">
                  History
                </h3>
                <span className="kicker">
                  last {detail.events.length} events
                </span>
              </div>
              {detail.events.length ? (
                <ul className="border border-ink-700 rounded-lg divide-y divide-ink-700/80 bg-ink-850">
                  {detail.events.map((e) => (
                    <li
                      key={e.seq}
                      className="flex flex-wrap items-baseline gap-2.5 px-3 py-2"
                    >
                      <span className="num text-micro text-ink-500 w-8 shrink-0">
                        #{e.seq}
                      </span>
                      <span className="num text-label text-ink-200 break-words">
                        {e.kind}
                      </span>
                      {(e.task_id || e.job_id) && (
                        <span className="num text-micro text-ink-500">
                          {e.task_id ?? e.job_id}
                        </span>
                      )}
                      <span className="num text-micro text-ink-500 ml-auto shrink-0">
                        {fmtTime(e.at)}
                      </span>
                    </li>
                  ))}
                </ul>
              ) : (
                <p className="text-secondary text-ink-500">No events yet.</p>
              )}
            </section>
          </>
        )}
        {profile && (
          <section>
            <h3 className="text-cardtitle font-semibold text-ink-100 mb-2">
              Model, effort & usage
            </h3>
            <ProfileBlock agent={profile} />
          </section>
        )}
        {resume && (
          <section>
            <h3 className="text-cardtitle font-semibold text-ink-100 mb-2">
              Resume command
            </h3>
            <p className="text-label text-ink-400 mb-2">
              {observed?.resume_hint ??
                "For use in a terminal by the operator."}
            </p>
            <pre className="agents-command num text-label text-accent bg-accent/10 rounded p-3">
              {resume}
            </pre>
          </section>
        )}
        <section>
          <div className="flex items-baseline gap-2 mb-2">
            <h3 className="text-cardtitle font-semibold text-ink-100">
              Actions
            </h3>
            <span className="kicker">read-only in this stage</span>
          </div>
          <div className="flex flex-wrap gap-1.5">
            {(fenced || deadHolding || lifecycle === "idle-holding") && (
              <>
                <Button
                  size="sm"
                  disabled
                  title="Resume is not wired from the board yet"
                >
                  Resume
                </Button>
                <Button
                  size="sm"
                  variant="danger"
                  disabled
                  title="Claim release is not wired from the board yet"
                >
                  Release claim
                </Button>
              </>
            )}
            {(fenced || deadHolding) && (
              <>
                <Button
                  size="sm"
                  disabled
                  title="Reassignment is not wired from the board yet"
                >
                  Reassign
                </Button>
                <Button
                  size="sm"
                  variant="danger"
                  disabled
                  title="Unfence runs from the To do rail, where the operator records the outcome"
                >
                  Unfence
                </Button>
              </>
            )}
            {(lifecycle === "stale-inbox" || (profile && profile.queued > 0)) && (
              <>
                <Button
                  size="sm"
                  disabled
                  title="Inbox draining is not wired from the board yet"
                >
                  Drain inbox
                </Button>
                <Button
                  size="sm"
                  variant="danger"
                  disabled
                  title="Archiving is not wired from the board yet"
                >
                  Archive
                </Button>
              </>
            )}
            {lifecycle === "active" && (
              <Button
                size="sm"
                variant="danger"
                disabled
                title="Stopping a turn is not wired from the board yet"
              >
                Stop
              </Button>
            )}
            {lifecycle === "normal" && !fenced && !(profile && profile.queued > 0) && (
              <p className="text-secondary text-ink-500">
                No lifecycle actions apply to this agent.
              </p>
            )}
          </div>
        </section>
      </div>
    </dialog>
  );
}
