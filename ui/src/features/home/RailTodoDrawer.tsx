import { useEffect, useState, type ReactNode } from "react";
import { api } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import { resources } from "../../lib/resources";
import type { AgentsPayload, DocumentFollowUp, Routine, RoutineRun, StagedRoutineProposal } from "../../lib/types";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import SafeLink from "../../ui/SafeLink";
import { useWriteBlock } from "../auth/WriteGate";
import DrawerShell, { type DrawerMenuItem } from "../app-shell/shared/DrawerShell";
import { useAssistantActor } from "../assistant/AssistantIdentity";
import { displayName } from "../assistant/identity";
import { routinesApi } from "../agents/routinesApi";
import AnswerForm, { READ_ONLY_COPY, useAnswer } from "./AnswerForm";
import PermissionCard from "./PermissionCard";
import { sendToMaster } from "./send";
import { cleanPermissionInline } from "./permissionFormat";
import {
  ageWords,
  fixPrompt,
  kindSpec,
  metaLine,
  needRefs,
  plainFailure,
  routineDraftReviewHref,
  routineReviewHref,
  UNFENCE_CHOICES,
  type HomeNeed,
  type UnfenceChoice,
} from "./needs";
import { todoPresentation, waitingRoutineReason } from "./railModel";
import { marksFor, sendingTodo, settleTodo, useTodoLocal } from "./todoLocal";
import { privateSessionGeneration } from "../../lib/privateSession";

export type RailTodoTarget =
  | { kind: "need"; key: string }
  | { kind: "routine_step_hold"; run: RoutineRun }
  | { kind: "routine_proposal"; proposal: StagedRoutineProposal }
  | { kind: "waiting_routine"; routine: Routine }
  | { kind: "document"; doc: DocumentFollowUp };

const SNOOZE_24H = 86_400;
const SNOOZE_7D = 604_800;

function copyText(text: string): Promise<void> {
  try {
    return navigator.clipboard.writeText(text);
  } catch (e) {
    return Promise.reject(e);
  }
}

function TechnicalDetails({ need, failure }: { need: HomeNeed; failure: string | null }) {
  const a = need.action;
  const rows: [string, string | null][] = [
    ["kind", need.kind],
    ["issue", need.issue],
    ["pr", a.type === "merge" ? a.pr : null],
    ["head", a.type === "merge" ? a.sha : null],
    ["reviewer", a.type === "merge" ? a.reviewer : null],
    ["agent", need.agent],
    ["owner", need.owner],
    ["command", a.type === "permission" ? a.argv : need.command],
    ["folder", a.type === "permission" && a.cwd ? a.cwd : null],
    ["risk", a.type === "permission" ? a.risk : null],
    ["row", need.title],
    ["last error", failure],
  ];
  return (
    <dl className="todo-tech mt-2" data-todo-details>
      {rows
        .filter((r): r is [string, string] => r[1] !== null && r[1] !== "")
        .map(([k, v]) => (
          <div key={k}>
            <dt>{k}</dt>
            <dd>{v}</dd>
          </div>
        ))}
    </dl>
  );
}

function TwoOptionAnswer({
  need,
  block,
  onDone,
}: {
  need: HomeNeed & { action: { type: "answer" } };
  block: string | null;
  onDone: (answer: string) => void;
}) {
  const t = useTranslate();
  const { send, busy, error } = useAnswer(need, onDone);
  const { impact, body, options } = need.action;
  return (
    <div className="space-y-3">
      {need.summary && <p className="text-secondary text-ink-300 whitespace-pre-wrap">{need.summary}</p>}
      {(body || impact) && (
        <div className="rounded border border-ink-700 bg-ink-900 p-3 space-y-1.5">
          {impact && <p className="text-micro text-ink-400">{t("Impact:")} {impact}</p>}
          {body && <pre className="whitespace-pre-wrap break-words text-micro text-ink-300">{body}</pre>}
        </div>
      )}
      <div className="flex flex-wrap gap-2" role="group" aria-label={t("your answer")}>
        {options.map((option) => (
          <Button
            key={option}
            size="sm"
            variant="primary"
            disabled={!!block || busy}
            title={block ? t(READ_ONLY_COPY) : `${t("Answer")}: ${option}`}
            onClick={() => send(option)}
          >
            {option}
          </Button>
        ))}
      </div>
      {error && (
        <p className="text-micro text-fail break-words" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

function NeedDrawerContent({
  need,
  readOnly,
  scopedHost,
  onOpenIssue,
  onAsk,
  onDone,
  onHide,
  onSent,
  onClose,
}: {
  need: HomeNeed;
  readOnly: boolean;
  scopedHost: boolean;
  onOpenIssue?: (id: string) => void;
  onAsk?: (need: HomeNeed) => void;
  onDone: (need: HomeNeed, text: string) => void;
  onHide: (need: HomeNeed) => void;
  onSent?: () => void;
  onClose: (restoreFocus?: boolean) => void;
}) {
  const t = useTranslate();
  const block = useWriteBlock(readOnly);
  const actor = useAssistantActor();
  const assistantName = displayName(actor.read, actor.scope, t);
  const spec = kindSpec(need);
  const pres = todoPresentation(need);
  const template = spec.title(need, { issueTitle: null });
  const title = need.shortTitle ?? template;
  const { sent, sending } = marksFor(useTodoLocal(), need);
  const [busy, setBusy] = useState(false);
  const [details, setDetails] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  const action = need.action;
  const answer = action.type === "answer" ? (need as HomeNeed & { action: { type: "answer" } }) : null;
  const twoOption = answer && answer.action.options.length === 2 ? answer : null;

  const refresh = () => {
    void resources.overview.invalidate();
    void resources.agents.invalidate();
  };

  const run = (work: () => Promise<unknown>, what: string, hide: boolean) => {
    if (block || busy) return;
    setBusy(true);
    setError(null);
    work()
      .then(() => {
        if (hide) onHide(need);
        else onDone(need, what);
        refresh();
        onClose();
      })
      .catch((e: unknown) => {
        setError(plainFailure(e));
        setFailure((e as Error)?.message ?? null);
      })
      .finally(() => setBusy(false));
  };

  const decide = (verb: "snooze" | "dismiss", secs?: number) => {
    const id = need.subject ?? { kind: "row", id: need.key };
    run(() => api.needDecide(verb, id.kind, id.id, secs), "", true);
  };

  const agentAct = (verb: "resume" | "unfence", status?: UnfenceChoice["status"]) => {
    if (!need.agent) return;
    run(
      () => api.agentAct(need.agent!, verb, status),
      verb === "resume" ? "Resumed" : `Recorded: ${UNFENCE_CHOICES.find((c) => c.status === status)?.plain ?? status}`,
      false,
    );
  };

  const fixIt = () => {
    if (block || sending) return;
    const generation = privateSessionGeneration();
    sendingTodo(need);
    setError(null);
    void sendToMaster(fixPrompt(need, template), undefined, needRefs(need)).then((r) => {
      if (generation !== privateSessionGeneration()) return;
      settleTodo(need, r.ok);
      if (r.ok) {
        onSent?.();
        onClose();
      } else {
        setError("It couldn't be sent to Master. Try again.");
        setFailure(r.error);
      }
    });
  };

  const menuItems: DrawerMenuItem[] = [
    ...(need.issue && !need.link && onOpenIssue
      ? [{ key: "open-issue", label: t("Open"), onSelect: () => { onOpenIssue(need.issue!); onClose(false); } }]
      : []),
    {
      key: "snooze-24h",
      label: t("Snooze 24h"),
      disabled: !!block || busy,
      title: block ?? undefined,
      onSelect: () => decide("snooze", SNOOZE_24H),
    },
    {
      key: "snooze-7d",
      label: t("Snooze 7d"),
      disabled: !!block || busy,
      title: block ?? undefined,
      onSelect: () => decide("snooze", SNOOZE_7D),
    },
    {
      key: "dismiss",
      label: t("Dismiss"),
      destructive: true,
      disabled: !!block || busy,
      title: block ?? undefined,
      onSelect: () => decide("dismiss"),
    },
    {
      key: "copy-command",
      label: t("Copy command"),
      onSelect: () => {
        copyText(need.command).then(
          () => {
            setNote(t("Copied"));
            setTimeout(() => setNote(null), 1500);
          },
          () => undefined,
        );
      },
    },
  ];

  const askLabel = scopedHost ? t("Ask {name}").replace("{name}", () => assistantName) : t("Ask Master");
  const isStuck = spec.place === "send" || (!answer && action.type !== "permission" && need.kind !== "fenced" && !(need.kind === "stopped" && !!need.agent));

  let body: ReactNode;
  if (action.type === "permission") {
    body = (
      <div className="space-y-3">
        <dl className="dx-kv-card">
          <div className="dx-kv-line">
            <dt>{t("Command")}</dt>
            <dd className="font-mono text-micro"><bdi>{cleanPermissionInline(action.argv)}</bdi></dd>
          </div>
          {action.cwd && (
            <div className="dx-kv-line">
              <dt>{t("Folder")}</dt>
              <dd className="font-mono text-micro"><bdi>{cleanPermissionInline(action.cwd)}</bdi></dd>
            </div>
          )}
        </dl>
        <PermissionCard
          compact
          card={action.risk ? action : { ...action, risk: "low" }}
          readOnly={readOnly}
          onDone={(txt) => {
            onDone(need, txt);
            onClose();
          }}
        />
      </div>
    );
  } else if (twoOption) {
    body = (
      <TwoOptionAnswer
        need={twoOption}
        block={block}
        onDone={(txt) => {
          onDone(need, `Answered: ${txt}`);
          onClose();
        }}
      />
    );
  } else if (answer) {
    body = (
      <AnswerForm
        need={answer}
        readOnly={readOnly}
        onDone={(txt) => {
          onDone(need, `Answered: ${txt}`);
          onClose();
        }}
      />
    );
  } else if (need.kind === "fenced" && need.agent) {
    body = (
      <div className="space-y-3">
        <p className="text-secondary text-ink-300">{t("How did its last piece of work end?")}</p>
        <div className="flex flex-wrap gap-2" role="group" aria-label={t("how the work ended")}>
          {UNFENCE_CHOICES.map((c) => (
            <Button
              key={c.status}
              size="sm"
              disabled={!!block || busy}
              title={block ? t(READ_ONLY_COPY) : c.blurb}
              onClick={() => agentAct("unfence", c.status)}
            >
              {c.plain}
            </Button>
          ))}
        </div>
        {block && <p className="text-micro text-ink-500">{t(READ_ONLY_COPY)}</p>}
      </div>
    );
  } else if (need.kind === "stopped" && need.agent) {
    body = (
      <div className="space-y-3">
        <p className="text-secondary text-ink-300">{t("It stopped with work still waiting.")}</p>
        <div className="flex flex-wrap gap-2">
          <Button
            size="sm"
            variant="primary"
            disabled={!!block || busy}
            loading={busy}
            title={block ? t(READ_ONLY_COPY) : undefined}
            onClick={() => agentAct("resume")}
          >
            {busy ? t("Working…") : t("Start it again")}
          </Button>
          {sent ? (
            <span className="text-label text-ok">{t("Sent to Master")}</span>
          ) : (
            <Button
              size="sm"
              variant="secondary"
              disabled={!!block || sending || busy}
              loading={sending}
              title={block ? t(READ_ONLY_COPY) : undefined}
              onClick={fixIt}
            >
              {t("Fix it")}
            </Button>
          )}
        </div>
        {block && <p className="text-micro text-ink-500">{t(READ_ONLY_COPY)}</p>}
      </div>
    );
  } else {
    body = (
      <div className="space-y-3">
        <p className="text-secondary text-ink-300 wrap-anywhere">
          {need.why ?? need.summary ?? (scopedHost
            ? t("{name} can walk you through this and take it from there.").replace("{name}", () => assistantName)
            : t("Master can walk you through this and take it from there."))}
        </p>
        <div className="flex flex-wrap items-center gap-2">
          {sent ? (
            <span className="text-label text-ok">{t("Sent to Master")}</span>
          ) : (
            <Button
              size="sm"
              variant="primary"
              disabled={!!block || sending || busy}
              loading={sending}
              title={block ? t(READ_ONLY_COPY) : undefined}
              onClick={fixIt}
            >
              {t("Fix it")}
            </Button>
          )}
          {need.link && (
            <SafeLink className="btn btn-sm btn-secondary" warnClassName="text-micro text-warn" href={need.link}>
              {t("Open")}
            </SafeLink>
          )}
        </div>
      </div>
    );
  }

  return (
    <DrawerShell
      kind="todo-detail"
      label={t(pres.kindLabel)}
      kicker={t("To do")}
      scrim="board"
      modal
      trapFocus
      title={title}
      subtitle={metaLine(need)}
      pills={
        <span className="s-status-pill" data-tone={pres.tone === "fail" ? "warn" : pres.tone}>
          {t(pres.kindLabel)}
        </span>
      }
      warning={block ? t(READ_ONLY_COPY) : undefined}
      menu={menuItems}
      secondary={
        !isStuck && onAsk ? (
          <Button size="sm" variant="ghost" disabled={!!block} title={block ? t(READ_ONLY_COPY) : askLabel} onClick={() => { onAsk(need); onClose(false); }}>
            {askLabel}
          </Button>
        ) : undefined
      }
      primary={
        <Button size="sm" variant="ghost" onClick={() => onClose()}>
          {t("Not now")}
        </Button>
      }
      tabs={[
        {
          id: "detail",
          label: t("Details"),
          panel: (
            <div className="dx-panel">
              {body}
              {error && (
                <p className="text-label text-fail break-words" role="alert">
                  {error}
                </p>
              )}
              {note && <p className="text-label text-ok">{note}</p>}
              <dl className="dx-kv-card">
                <div className="dx-kv-line">
                  <dt>{t("Source")}</dt>
                  <dd>{pres.sourceLabel}</dd>
                </div>
                {need.project && need.project !== "all" && (
                  <div className="dx-kv-line">
                    <dt>{t("Project")}</dt>
                    <dd>{need.project}</dd>
                  </div>
                )}
                <div className="dx-kv-line">
                  <dt>{t("Waiting")}</dt>
                  <dd className="num">{ageWords(need.age)}</dd>
                </div>
              </dl>
              <div>
                <button type="button" className="lnk text-label" aria-expanded={details} onClick={() => setDetails((d) => !d)}>
                  {t("Details")}
                </button>
                {details && <TechnicalDetails need={need} failure={failure} />}
              </div>
            </div>
          ),
        },
      ]}
      onClose={onClose}
    />
  );
}

/**
 * Shared Drawer for all To do cards that are not handled by `StagedWriteDetail`,
 * `WorkItemDetail` (plan / idea / merge), or `HostedTemplateReview`.
 */
export default function RailTodoDrawer({
  target,
  needs,
  readOnly,
  scopedHost,
  hostedRoster,
  onOpenIssue,
  onAsk,
  onDone,
  onHide,
  onSent,
  onRoutineDecided,
  onClose,
}: {
  target: RailTodoTarget | null;
  needs: readonly HomeNeed[];
  readOnly: boolean;
  scopedHost: boolean;
  hostedRoster?: AgentsPayload | null;
  onOpenIssue?: (id: string) => void;
  onAsk?: (need: HomeNeed) => void;
  onDone: (need: HomeNeed, text: string) => void;
  onHide: (need: HomeNeed) => void;
  onSent?: () => void;
  onRoutineDecided: (id: string) => void;
  onClose: (restoreFocus?: boolean) => void;
}) {
  const t = useTranslate();
  const block = useWriteBlock(readOnly);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setBusy(false);
    setError(null);
  }, [target]);

  if (!target) return null;

  if (target.kind === "need") {
    const need = needs.find((n) => n.key === target.key);
    if (!need) return null;
    return (
      <NeedDrawerContent
        need={need}
        readOnly={readOnly}
        scopedHost={scopedHost}
        onOpenIssue={onOpenIssue}
        onAsk={onAsk}
        onDone={onDone}
        onHide={onHide}
        onSent={onSent}
        onClose={onClose}
      />
    );
  }

  if (target.kind === "routine_step_hold") {
    const run = target.run;
    const href = routineReviewHref(run.agent_alias, hostedRoster);
    const waitingStep =
      (run.checklist_state ?? run.checklist ?? []).find((s) => s.state === "waiting_approval")?.label ??
      t("Ask for approval first");
    const decide = async (decision: "approve" | "dismiss") => {
      if (busy || readOnly) return;
      setBusy(true);
      setError(null);
      try {
        await routinesApi.runDecide(run.id, decision);
        onRoutineDecided(run.id);
        void resources.overview.refresh();
        onClose();
      } catch {
        setError(t("Could not record step decision."));
      } finally {
        setBusy(false);
      }
    };
    return (
      <DrawerShell
        kind="todo-detail"
        label={t("Approval")}
        kicker={t("To do")}
        scrim="board"
        modal
        trapFocus
        title={run.routine_name ?? run.routine_id}
        subtitle={run.agent_alias ? `${run.agent_alias} · ${t("Approval")}` : t("Approval")}
        pills={<span className="s-status-pill" data-tone="warn">{t("Approval")}</span>}
        warning={block ? t(READ_ONLY_COPY) : undefined}
        secondary={
          <>
            {href && (
              <Link href={href} className="btn btn-sm btn-secondary" onClick={() => onClose(false)}>
                {t("Open routine")} ↗
              </Link>
            )}
            <Button size="sm" variant="secondary" disabled={!!block || busy} onClick={() => void decide("dismiss")}>
              {t("Dismiss")}
            </Button>
          </>
        }
        primary={
          <Button size="sm" variant="primary" loading={busy} disabled={!!block || busy} onClick={() => void decide("approve")}>
            {t("Approve & send")}
          </Button>
        }
        tabs={[
          {
            id: "detail",
            label: t("Details"),
            panel: (
              <div className="dx-panel">
                <p className="text-secondary text-ink-200">{waitingStep}</p>
                {error && <p className="text-label text-fail" role="alert">{error}</p>}
              </div>
            ),
          },
        ]}
        onClose={onClose}
      />
    );
  }

  if (target.kind === "routine_proposal") {
    const p = target.proposal;
    const href = routineDraftReviewHref(p, hostedRoster);
    const decide = async (decision: "approve" | "dismiss") => {
      if (busy || readOnly) return;
      setBusy(true);
      setError(null);
      try {
        await routinesApi.proposalDecide(p.id, decision);
        onRoutineDecided(p.id);
        void resources.overview.refresh();
        onClose();
      } catch {
        setError(t("Could not record routine proposal decision."));
      } finally {
        setBusy(false);
      }
    };
    return (
      <DrawerShell
        kind="todo-detail"
        label={t("Approval")}
        kicker={t("To do")}
        scrim="board"
        modal
        trapFocus
        title={p.title}
        subtitle={p.kind === "wiki_propose" ? t("Playbook draft") : t("Routine draft")}
        pills={<span className="s-status-pill" data-tone="warn">{t("Approval")}</span>}
        warning={block ? t(READ_ONLY_COPY) : undefined}
        secondary={
          <>
            {href && (
              <Link href={href} className="btn btn-sm btn-secondary" onClick={() => onClose(false)}>
                {t("Review")} ↗
              </Link>
            )}
            <Button size="sm" variant="secondary" disabled={!!block || busy} onClick={() => void decide("dismiss")}>
              {t("Dismiss")}
            </Button>
          </>
        }
        primary={
          <Button size="sm" variant="primary" loading={busy} disabled={!!block || busy} onClick={() => void decide("approve")}>
            {t("Approve")}
          </Button>
        }
        tabs={[
          {
            id: "detail",
            label: t("Details"),
            panel: (
              <div className="dx-panel">
                <dl className="dx-kv-card">
                  <div className="dx-kv-line">
                    <dt>{t("Kind")}</dt>
                    <dd>{p.kind === "wiki_propose" ? t("Playbook draft") : t("Routine draft")}</dd>
                  </div>
                  {p.proposed_by && (
                    <div className="dx-kv-line">
                      <dt>{t("Proposed by")}</dt>
                      <dd>{p.proposed_by}</dd>
                    </div>
                  )}
                </dl>
                {error && <p className="text-label text-fail" role="alert">{error}</p>}
              </div>
            ),
          },
        ]}
        onClose={onClose}
      />
    );
  }

  if (target.kind === "waiting_routine") {
    const r = target.routine;
    const href = routineReviewHref(r.agent_alias, hostedRoster);
    const reason = waitingRoutineReason(r, t);
    return (
      <DrawerShell
        kind="todo-detail"
        label={t("Review")}
        kicker={t("To do")}
        scrim="board"
        modal
        trapFocus
        title={r.name}
        subtitle={r.agent_alias}
        pills={<span className="s-status-pill" data-tone="info">{t("Review")}</span>}
        secondary={<Button size="sm" variant="ghost" onClick={() => onClose()}>{t("Not now")}</Button>}
        primary={href ? <Link href={href} className="btn btn-sm btn-primary" onClick={() => onClose(false)}>{t("Open routine")} ↗</Link> : undefined}
        tabs={[{
          id: "detail",
          label: t("Details"),
          panel: <div className="dx-panel"><p className="text-secondary text-ink-300">{reason}</p></div>,
        }]}
        onClose={onClose}
      />
    );
  }

  const doc = target.doc;
  const href = `/wiki?file=${encodeURIComponent(doc.id)}&version=${doc.version}`;
  return (
    <DrawerShell
      kind="todo-detail"
      label={t("Review")}
      kicker={t("To do")}
      scrim="board"
      modal
      trapFocus
      title={doc.title}
      subtitle={`${doc.project ? `${doc.project} · ` : ""}v${doc.version}`}
      pills={<span className="s-status-pill" data-tone="info">{t("Approval requested")}</span>}
      secondary={<Button size="sm" variant="ghost" onClick={() => onClose()}>{t("Not now")}</Button>}
      primary={<Link href={href} className="btn btn-sm btn-primary" onClick={() => onClose(false)}>{t("Open in Wiki")} ↗</Link>}
      tabs={[{
        id: "detail",
        label: t("Details"),
        panel: (
          <div className="dx-panel">
            <dl className="dx-kv-card">
              <div className="dx-kv-line"><dt>{t("Version")}</dt><dd className="num">v{doc.version}</dd></div>
              {doc.project && <div className="dx-kv-line"><dt>{t("Project")}</dt><dd>{doc.project}</dd></div>}
              <div className="dx-kv-line"><dt>{t("Status")}</dt><dd>{t("Approval requested")}</dd></div>
            </dl>
          </div>
        ),
      }]}
      onClose={onClose}
    />
  );
}
