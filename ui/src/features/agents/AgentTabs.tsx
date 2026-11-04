import { useEffect, useState } from "react";
import { api } from "../../lib/api";
import { ApiError } from "../../lib/apiError";
import { fmtTime } from "../../lib/fmt";
import { navigate } from "../../lib/useLocation";
import { setComposerDraft } from "../app-shell/chat/composerStore";
import { useTranslate } from "../../lib/locale";
import Button from "../../ui/Button";
import type { ToastMsg } from "../../ui/Toast";
import DrawerShell from "../app-shell/shared/DrawerShell";
import type { Agent, AgentAccess, AgentDetail, AgentRole, HiredProfile } from "../../lib/types";
import { AgentActivity, AgentQueue, AssignedTaskList, WorkBlock, agentThreadHref, detailTasksNotOnRoster, partitionActiveTasks } from "./AgentBlocks";
import Link from "../../ui/Link";
import { ProfileBlock } from "./AgentProfile";
import { MASTER } from "../home/master";
import InstructionsEditor from "./InstructionsEditor";
import { contextOf, nativeChange } from "./instructionsDoc";
import type { InstructionsDoc } from "./useInstructionsDoc";
import {
  BUNDLED_SKILLS,
  HISTORY_FILTERS,
  historyKind,
  type AgentTab,
  type HistoryFilter,
} from "./agentTabModel";
import { agentLocation } from "./agentView";

export interface TabContext {
  alias: string;
  profile: Agent | undefined;
  observed: Agent | undefined;
  detail: AgentDetail | null;
  role: AgentRole | null;
  hosted: boolean;
  resume: string | null | undefined;
  onOpenIssue: (id: string) => void;
  /** Set when this page is a profile with no runtime: nothing to read but itself. */
  hired: HiredProfile | undefined;
  /** This viewer may save, and the board can take the write now. */
  writable: boolean;
  toast: (kind: ToastMsg["kind"], text: string) => void;
  /** Label and value of the agent's figures; "—" is not recorded. */
  stats: [string, string][];
  /** The detail read failed. Roster tasks, when present, stay visible. */
  detailFailed: boolean;
  /** This board answers the read with 501: it keeps no such details. That is
   *  permanent, so it reads as neither a failure to retry nor an empty record. */
  detailNotServed: boolean;
  retryDetail: () => void;
  /** Whether the role is known yet. A profile that names none has nothing to wait for. */
  roleRead: "read" | "reading" | "failed";
  retryRole: () => void;
  /** The Instructions document: draft, revision and save state. It lives on the
   *  page, so a tab trip keeps the draft and the caret. */
  doc: InstructionsDoc;
}

const NOT_RECORDED = "This board doesn’t record agent history.";

/** No answer yet: a read still running says nothing about what the agent has. */
function reading(c: TabContext): boolean {
  return !c.detail && !c.detailFailed && !c.detailNotServed;
}

function Panel({ title, mark, children }: { title: string; mark?: string; children: React.ReactNode }) {
  const t = useTranslate();
  return (
    <section data-work-panel={mark}>
      <div className="ap-panel-h">
        <h3>{t(title)}</h3>
      </div>
      {children}
    </section>
  );
}

function Rows({ children }: { children: React.ReactNode }) {
  return <div className="ap-rows">{children}</div>;
}

function Row({ left, right }: { left: React.ReactNode; right?: React.ReactNode }) {
  return (
    <div className="ap-row">
      <span>{left}</span>
      {right !== undefined && <small className="num">{right}</small>}
    </div>
  );
}

/** A list row is a name and one line; the details live in its drawer. */
function ManageRow({ id, name, line, onOpen }: { id: string; name: string; line: string; onOpen: () => void }) {
  return (
    <button type="button" className="ap-mrow" data-record-id={id} onClick={onOpen}>
      <b>{name}</b>
      {line && <small>{line}</small>}
    </button>
  );
}

type Fact = [string, string | null | undefined];

function Facts({ items }: { items: Fact[] }) {
  const t = useTranslate();
  return (
    <dl className="rc-kv">
      {items
        .filter(([, value]) => value !== null && value !== undefined && value !== "")
        .map(([label, value]) => (
          <div key={label} className="contents">
            <dt>{t(label)}</dt>
            <dd>{value}</dd>
          </div>
        ))}
    </dl>
  );
}

function ItemDrawer({ title, onClose, children }: { title: string; onClose: () => void; children: React.ReactNode }) {
  return (
    <DrawerShell
      kind="agent-item"
      label={title}
      title={title}
      tabs={[{ id: "details", label: title, panel: <div className="ap-body">{children}</div> }]}
      onClose={onClose}
    />
  );
}

function Note({ children }: { children: React.ReactNode }) {
  return <p className="rc-note">{children}</p>;
}

function Failed({ text, onRetry }: { text: string; onRetry: () => void }) {
  const t = useTranslate();
  return (
    <p className="rc-note" role="alert">
      {t(text)}{" "}
      <button type="button" className="lnk" onClick={onRetry}>
        {t("Retry")}
      </button>
    </p>
  );
}

/** A role still unread says nothing about what it names. */
function RoleRead({ c }: { c: TabContext }) {
  const t = useTranslate();
  if (c.roleRead === "read") return null;
  if (c.roleRead === "reading") return <Note>{t("Loading…")}</Note>;
  return <Failed text="This agent’s role couldn’t be read." onRetry={c.retryRole} />;
}

/** First-job chips: open the agent's thread with the text in the composer.
 *  Nothing is sent; the operator reviews it first. */
export function FirstJobs({ alias, tasks }: { alias: string; tasks: string[] }) {
  const t = useTranslate();
  return (
    <div className="rc-sugs" style={{ marginTop: "0.5rem" }}>
      {tasks.map((task) => (
        <button
          key={task}
          type="button"
          className="rc-sug"
          title={t("Opens the thread with this text ready to send")}
          onClick={() => {
            setComposerDraft(`threads:${alias}`, task);
            navigate(`/threads?dm=${encodeURIComponent(alias)}`);
          }}
        >
          {task}
        </button>
      ))}
    </div>
  );
}

function NowCard({ c, onlyDoing = false }: { c: TabContext; onlyDoing?: boolean }) {
  const t = useTranslate();
  const parts = c.observed ? partitionActiveTasks(c.observed) : null;
  if (c.detailFailed && !c.observed) {
    return (
      <div className="ap-now">
        <b>{t("Agent details are unavailable.")}</b>
      </div>
    );
  }
  if (reading(c) && !c.observed) return <Note>{t("Loading…")}</Note>;
  const working = Boolean(
    c.observed &&
      (c.observed.running > 0 ||
        (onlyDoing ? (parts?.doing.length ?? 0) > 0 : (c.observed.tasks?.length ?? 0) > 0)),
  );
  if (c.observed && working) {
    return (
      <div className="ap-now">
        <WorkBlock
          agent={c.observed}
          onOpenIssue={c.onOpenIssue}
          tasks={onlyDoing ? parts?.doing : undefined}
        />
        <div className="rc-sugs" style={{ marginTop: "0.5rem" }}>
          <Button size="sm" href={agentThreadHref(c.alias)}>
            {t("Open thread")}
          </Button>
        </div>
      </div>
    );
  }
  if (onlyDoing && parts && (parts.queue.length > 0 || parts.attention.length > 0)) {
    return (
      <div className="ap-now">
        <b>{t("Nothing right now")}</b>
      </div>
    );
  }
  return (
    <div className="ap-now">
      <b>{t("Nothing right now")}</b>
      {c.writable && c.role && c.role.first_tasks.length > 0 && (
        <>
          <small>{t("Give it a job:")}</small>
          <FirstJobs alias={c.alias} tasks={c.role.first_tasks} />
        </>
      )}
    </div>
  );
}

function resultRows(detail: AgentDetail | null) {
  return (detail?.events ?? []).filter((e) => /finished|report|completed|merged/.test(e.kind));
}

function EventRow({ e }: { e: AgentDetail["events"][number] }) {
  return (
    <Row
      left={
        <>
          <span className="num">#{e.seq}</span> {e.kind}
          {(e.task_id || e.job_id) && <small> {e.task_id ?? e.job_id}</small>}
        </>
      }
      right={fmtTime(e.at)}
    />
  );
}

function attentionNotes(agent: TabContext["observed"], t: (english: string) => string): string[] {
  if (!agent) return [];
  const notes: string[] = [];
  if (agent.fenced) notes.push(t("Recovery required"));
  if (agent.unknown > 0) notes.push(t("{count} outcome unknown").replace("{count}", String(agent.unknown)));
  if (agent.stalled) {
    const secs = agent.silent_secs ?? 0;
    const time = secs < 60 ? `${Math.floor(secs)}s` : secs < 3600 ? `${Math.floor(secs / 60)}m` : `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
    notes.push(t("Stalled · {time}").replace("{time}", () => time));
  }
  if (agent.state === "waiting_input") notes.push(t("Waiting for input"));
  if (
    agent.state === "blocked_by_quota" ||
    agent.quota?.state === "blocked" ||
    agent.usage_limit?.state === "blocked"
  ) notes.push(t("blocked by quota"));
  return notes;
}

export function Work({ c }: { c: TabContext }) {
  const t = useTranslate();
  const detail = c.detail;
  const results = resultRows(detail);
  const parts = c.observed ? partitionActiveTasks(c.observed) : null;
  const detailOnly = detailTasksNotOnRoster(parts, detail?.tasks);
  const notes = attentionNotes(c.observed, t);
  const unavailable = c.detailFailed && !c.observed;
  const attentionCount = notes.length + (parts?.attention.length ?? 0);
  return (
    <div className="ap-body">
      <Panel title="Doing now" mark="doing">
        <NowCard c={c} onlyDoing />
        {c.observed && (
          <div className="mt-2">
            <AgentActivity agent={c.observed} />
          </div>
        )}
      </Panel>
      <Panel title="Queue" mark="queue">
        {c.observed ? <AgentQueue agent={c.observed} /> : <Note>{t(unavailable ? "Agent details are unavailable." : reading(c) ? "Loading…" : "Nothing queued.")}</Note>}
        {parts && <AssignedTaskList alias={c.alias} tasks={parts.queue} />}
        {detailOnly.length > 0 && (
          <div className="mt-3">
            <h4 className="slabel mb-2">{t("Tasks")} · {t("assigned in flight")}</h4>
            <Rows>
              {detailOnly.map((task) => (
                <Row
                  key={task}
                  left={
                    <Link className="lnk num" href={agentThreadHref(c.alias)} data-task={task}>
                      {task}
                    </Link>
                  }
                />
              ))}
            </Rows>
          </div>
        )}
        {(detail?.on?.length ?? 0) > 0 && (
          <div className="mt-3">
            <h4 className="slabel mb-2">{t("Issues")} · {t("bound through the job")}</h4>
            <div className="flex flex-wrap gap-1.5">
              {detail!.on!.map((id) => (
                <button key={id} type="button" className="lnk" onClick={() => c.onOpenIssue(id)}>
                  {id}
                </button>
              ))}
            </div>
          </div>
        )}
      </Panel>
      <Panel title="Needs attention" mark="attention">
        {notes.map((note) => (
          <p key={note} className="text-label text-warn">{note}</p>
        ))}
        {parts && <AssignedTaskList alias={c.alias} tasks={parts.attention} />}
        {attentionCount === 0 && (
          <Note>{t(unavailable ? "Needs attention could not be read." : reading(c) && !c.observed ? "Loading…" : "Nothing needs attention.")}</Note>
        )}
      </Panel>
      <Panel title="Results" mark="results">
        {c.detailNotServed ? (
          <Note>{t(NOT_RECORDED)}</Note>
        ) : c.detailFailed && !detail ? (
          <Note>{t("Agent details are unavailable.")}</Note>
        ) : detail?.events_error && results.length === 0 ? (
          <p role="alert" className="text-label text-fail">
            {t("The event history could not be read:")} {detail.events_error}
          </p>
        ) : results.length ? (
          <Rows>
            {results.map((e) => (
              <EventRow key={e.seq} e={e} />
            ))}
          </Rows>
        ) : (
          <Note>{t(reading(c) ? "Loading…" : "No results yet.")}</Note>
        )}
      </Panel>
    </div>
  );
}

function History({ c }: { c: TabContext }) {
  const t = useTranslate();
  const [filter, setFilter] = useState<HistoryFilter>("all");
  const [open, setOpen] = useState<string | null>(null);
  const detail = c.detail;
  if (c.hired) {
    const changed = c.doc.kind === "hosted" ? c.doc.profile : null;
    return (
      <div className="ap-body">
        {changed && (
          <>
            <Rows>
              <Row
                left={`${t("Profile last changed")} · ${t("revision")} ${changed.revision}`}
                right={fmtTime(new Date(changed.updated_at).toISOString())}
              />
            </Rows>
            <Note>{t("This board records when the profile last changed, not who changed it or which field.")}</Note>
          </>
        )}
        <Note>{t("No other history yet: it has no runtime.")}</Note>
      </div>
    );
  }
  const running = (detail?.running ?? []).map((m) => ({
    key: `m-${m.id}`,
    name: t("Turn running"),
    line: m.summary ?? String(m.task ?? m.id),
    group: "turns" as const,
    facts: [
      ["Message", m.id],
      ["Task", m.task],
      ["Started", m.created ? fmtTime(m.created) : null],
    ] as Fact[],
  }));
  const events = (detail?.events ?? []).map((e) => {
    // Instructions edits record who made them; say so, or say it is not recorded.
    const edit = nativeChange(e);
    return {
      key: `e-${e.seq}`,
      name: edit ? t("Instructions edited") : e.kind,
      line: [
        edit ? (edit.by ? t("By {actor}").replace("{actor}", () => edit.by as string) : t("Who isn’t recorded")) : (e.task_id ?? e.job_id),
        fmtTime(e.at),
      ]
        .filter(Boolean)
        .join(" · "),
      group: historyKind(e.kind),
      facts: [
        ["Event", `#${e.seq}`],
        ["Task", e.task_id],
        ["Job", e.job_id],
        ["By", edit?.by],
        ["When", fmtTime(e.at)],
      ] as Fact[],
    };
  });
  const all = [...running, ...events];
  const rows = all.filter((r) => filter === "all" || r.group === filter);
  const shown = all.find((r) => r.key === open);
  return (
    <div className="ap-body">
      {!c.detailNotServed && (
        <div className="rc-cats" role="group" aria-label={t("Filter history")}>
          {HISTORY_FILTERS.map((f) => (
            <button
              key={f.id}
              type="button"
              className="rc-cat"
              aria-pressed={filter === f.id}
              onClick={() => setFilter(f.id)}
            >
              {t(f.label)}
            </button>
          ))}
        </div>
      )}
      {detail?.events_error && (
        <p role="alert" className="text-label text-fail">
          {t("The event history could not be read:")} {detail.events_error}
        </p>
      )}
      {c.detailNotServed ? (
        <Note>{t(NOT_RECORDED)}</Note>
      ) : c.detailFailed && !detail ? (
        <Note>{t("Agent details are unavailable.")}</Note>
      ) : rows.length ? (
        <Rows>
          {rows.map((r) => (
            <ManageRow key={r.key} id={r.key} name={r.name} line={r.line} onOpen={() => setOpen(r.key)} />
          ))}
        </Rows>
      ) : (
        <Note>{t(reading(c) ? "Loading…" : "No events yet.")}</Note>
      )}
      {!c.detailNotServed && <Note>{t("Recent events only. An older edit may not be listed.")}</Note>}
      {shown && (
        <ItemDrawer title={shown.name} onClose={() => setOpen(null)}>
          <Facts items={shown.facts} />
        </ItemDrawer>
      )}
    </div>
  );
}

function Instructions({ c }: { c: TabContext }) {
  const t = useTranslate();
  const doc = c.doc;
  const context = contextOf(doc.draft ?? doc.served ?? "", doc.kind, doc.applies);
  const editor = (reason?: string) => (
    <div className="ap-body">
      {reason && <Note>{t(reason)}</Note>}
      <InstructionsEditor
        doc={doc}
        writable={c.writable && doc.kind !== null}
        resetTo={c.role?.instructions ?? null}
        contextHeading={context.heading}
        contextLines={context.lines}
        contextNote={
          <p className="ap-drawer-lead">
            {t("Skills in full, wiki pages the agent reads, earlier sessions, and a repository’s own agent instructions file during project work.")}
          </p>
        }
      />
    </div>
  );
  if (c.hired) {
    if (doc.kind === "hosted") {
      if (doc.read === "failed") return <Failed text="The instructions could not be read." onRetry={doc.retry} />;
      if (doc.read !== "ready") return <Note>{t("Loading instructions…")}</Note>;
      return editor();
    }
    return c.hired.job ? (
      <p className="rc-profile-job">{c.hired.job}</p>
    ) : (
      <Note>{t("There are no instructions to show for this agent.")}</Note>
    );
  }
  if (c.detailNotServed) {
    if (c.role?.instructions) return <p className="rc-profile-job">{c.role.instructions}</p>;
    return <Note>{c.role ? t("This agent was hired as {role}.").replace("{role}", () => c.role!.title) : t("This board doesn’t show job text for this agent.")}</Note>;
  }
  if (c.detailFailed && !c.detail) return <Note>{t("Agent details are unavailable.")}</Note>;
  if (!c.detail) return <Note>{t("Loading agent details…")}</Note>;
  if (doc.served === null) {
    return (
      <Note>
        {c.role
          ? t("The job text is visible to the operator. This agent was hired as {role}.").replace(
              "{role}",
              () => c.role!.title,
            )
          : t("This agent's instructions are not shown by the board, so they cannot be edited here.")}
      </Note>
    );
  }
  return editor(
    doc.kind === null
      ? c.alias === MASTER
        ? "The master’s instructions are not edited here."
        : "This agent’s job comes from its settings profile, which this board cannot edit."
      : undefined,
  );
}

function Skills({ c }: { c: TabContext }) {
  const t = useTranslate();
  const [open, setOpen] = useState<string | null>(null);
  const named = c.role?.skills ?? [];
  // A runtime loads every bundled skill. A profile has no runtime, so all that
  // is known is what its role names, and a hand-written one names nothing.
  const bundled = c.hired ? [] : BUNDLED_SKILLS.map((skill) => skill.name as string);
  const names = [...bundled, ...named.filter((name) => !bundled.includes(name))];
  const line = (name: string) =>
    BUNDLED_SKILLS.find((skill) => skill.name === name)?.line ?? "Named by its role";
  if (names.length === 0 && c.roleRead === "read") return <Note>{t("No skills are recorded for this agent.")}</Note>;
  return (
    <div className="ap-body">
      {names.length > 0 && (
        <Rows>
          {names.map((name) => (
            <ManageRow key={name} id={`skill-${name}`} name={name} line={t(line(name))} onOpen={() => setOpen(name)} />
          ))}
        </Rows>
      )}
      <RoleRead c={c} />
      {open && (
        <ItemDrawer title={open} onClose={() => setOpen(null)}>
          <p className="ap-drawer-lead">{t(line(open))}</p>
          <Facts
            items={[
              ["Source", t(bundled.includes(open) ? "Bundled" : "Role")],
              ["Loaded", t(bundled.includes(open) ? "In every session" : "Not recorded")],
              ["Named by its role", c.role ? t(named.includes(open) ? "Yes" : "No") : null],
            ]}
          />
        </ItemDrawer>
      )}
    </div>
  );
}

function Tools({ c }: { c: TabContext }) {
  const t = useTranslate();
  const [open, setOpen] = useState<string | null>(null);
  const BUILT_IN = "cadence";
  const line = (name: string) => (name === BUILT_IN ? "Tickets, threads and reports" : "Named by its role, not connected");
  const names = [BUILT_IN, ...(c.role?.tools ?? []).filter((name) => name !== BUILT_IN)];
  return (
    <div className="ap-body">
      <Rows>
        {names.map((name) => (
          <ManageRow key={name} id={`tool-${name}`} name={name} line={t(line(name))} onOpen={() => setOpen(name)} />
        ))}
      </Rows>
      <RoleRead c={c} />
      {open && (
        <ItemDrawer title={open} onClose={() => setOpen(null)}>
          <p className="ap-drawer-lead">{t(line(open))}</p>
          <Facts items={[["Source", t(open === BUILT_IN ? "Built in" : "Role")]]} />
        </ItemDrawer>
      )}
    </div>
  );
}

function ModelSection({ c }: { c: TabContext }) {
  const t = useTranslate();
  if (c.hired) {
    return (
      <Panel title="Model">
        <Facts items={[["Model", c.hired.model === "inherit" ? t("Inherit from the runtime") : c.hired.model]]} />
      </Panel>
    );
  }
  return (
    <Panel title="Model, effort & usage">
      {c.profile ? <ProfileBlock agent={c.profile} /> : <Note>{t(c.detailFailed ? "Agent details are unavailable." : "Loading agent details…")}</Note>}
    </Panel>
  );
}

function RuntimeSection({ c }: { c: TabContext }) {
  const t = useTranslate();
  if (c.hired) return null;
  const a = c.detail?.agent;
  const identity: [string, unknown][] = a
    ? [
        ["thread", a.thread_id],
        ["session", a.session_id],
        ["endpoint", a.endpoint],
        ["pid", a.pid],
        ["model", a.model],
        ["generation", a.generation],
        ["cwd", a.cwd],
        ["sandbox", a.sandbox],
      ]
    : [];
  const shown = (v: unknown) => v !== null && v !== undefined && v !== "";
  return (
    <Panel title="Runtime">
      {a ? (
        <dl className="rc-kv">
          <dt>{t("runtime")}</dt>
          <dd>
            {a.provider} / {a.endpoint_kind} ·{" "}
            {c.observed && agentLocation(c.observed) === "cloud"
              ? t("cloud")
              : c.hosted
                ? t("agenticos gateway")
                : t("local")}
          </dd>
          <dt>{t("home")}</dt>
          <dd>{a.cwd ?? "—"}</dd>
          <dt>{t("team role")}</dt>
          <dd>{a.team_role ?? c.observed?.team_role ?? "—"}</dd>
          <dt>{t("profile")}</dt>
          <dd>{typeof a.params?.profile === "string" ? a.params.profile : "—"}</dd>
          {identity
            .filter(([, v]) => shown(v))
            .map(([k, v]) => (
              <div key={k} className="contents">
                <dt>{k}</dt>
                <dd title={String(v)}>{String(v)}</dd>
              </div>
            ))}
          {Object.entries(a.params ?? {})
            .filter(([k]) => k !== "profile")
            .map(([k, v]) => (
              <div key={k} className="contents">
                <dt title={`param ${k}`}>{k}</dt>
                <dd title={typeof v === "string" ? v : JSON.stringify(v)}>
                  {typeof v === "string" ? v : JSON.stringify(v)}
                </dd>
              </div>
            ))}
        </dl>
      ) : c.detailNotServed ? (
        <Note>{t("This board doesn’t report runtime details.")}</Note>
      ) : c.detailFailed ? (
        <Failed text="Agent details are unavailable." onRetry={c.retryDetail} />
      ) : (
        <Note>{t("Loading agent details…")}</Note>
      )}
      {c.resume && (
        <>
          <p className="text-label text-ink-400 my-2">
            {c.observed?.resume_hint ?? t("For use in a terminal by the operator.")}
          </p>
          <pre className="ap-code">{c.resume}</pre>
        </>
      )}
    </Panel>
  );
}

function KnowledgeSection({ c }: { c: TabContext }) {
  const t = useTranslate();
  const project = c.detail?.agent.turn_project ?? null;
  // A hosted profile's folder is keyed by its id, and the board names it. Only
  // a native agent's folder is built from its alias.
  const root = c.hired?.wiki_root ?? `agents/${c.alias}`;
  const folder = `${root}/knowledge`;
  return (
    <Panel title="Knowledge">
      <Rows>
        <Row left={t("Workspace wiki")} right="global/" />
        <Row
          left={t("Project notes of the ticket it works on")}
          right={project ? `projects/${project}/` : t("projects/<key>/ — only while on that ticket")}
        />
        <div className="ap-row">
          <span className="num min-w-0 break-words">{folder}</span>
          <Button variant="ghost" size="sm" href={`/wiki/${folder.split("/").map(encodeURIComponent).join("/")}`}>
            {t("Open")}
          </Button>
        </div>
      </Rows>
    </Panel>
  );
}

/** The gear: everything about the agent that isn't one of its sections. */
export function SettingsDrawer({ c, onClose }: { c: TabContext; onClose: () => void }) {
  const t = useTranslate();
  const body = (
    <div className="ap-body">
      <ModelSection c={c} />
      {c.stats.some(([, value]) => value !== "—") && (
        <Panel title="Figures">
          <Facts items={c.stats.map(([label, value]) => [label, value])} />
        </Panel>
      )}
      <RuntimeSection c={c} />
      <KnowledgeSection c={c} />
      {!c.hired && <Access c={c} />}
    </div>
  );
  return (
    <DrawerShell
      kind="agent-settings"
      label={t("Agent settings")}
      title={t("Settings")}
      subtitle={c.alias}
      tabs={[{ id: "settings", label: t("Settings"), panel: body }]}
      onClose={onClose}
    />
  );
}

/**
 * CAD-1085: durable Capabilities, Connections and Access & security on the
 * agent page. Every fact is read from the daemon (grants and connection
 * metadata) or the already-loaded agent detail; custody facts are stated as
 * reported, and a locked key makes connection metadata unavailable rather
 * than claiming a credential works.
 */
function Access({ c }: { c: TabContext }) {
  const t = useTranslate();
  const [access, setAccess] = useState<AgentAccess | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [notServed, setNotServed] = useState(false);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let live = true;
    setAccess(null);
    setFailure(null);
    setNotServed(false);
    api
      .agentAccess(c.alias)
      .then((data) => {
        if (live) setAccess(data);
      })
      .catch((error: unknown) => {
        if (!live) return;
        if (error instanceof ApiError && error.status === 501 && error.code === "not_implemented") setNotServed(true);
        else setFailure(error instanceof ApiError ? error.message : String(error));
      });
    return () => {
      live = false;
    };
  }, [c.alias, attempt]);
  const capabilities = c.detail?.agent.capabilities ?? null;
  const capabilityNames = capabilities ? Object.keys(capabilities).sort() : [];
  const custody = access?.custody ?? null;
  const isolated = custody !== null && custody.isolation !== "none";
  return (
    <div className="space-y-4">
      <section>
        <div className="ap-panel-h">
          <h3>{t("Capabilities")}</h3>
        </div>
        {capabilityNames.length > 0 ? (
          <ul className="text-label text-ink-300 space-y-0.5">
            {capabilityNames.map((name) => (
              <li key={name} className="num break-words">{name}</li>
            ))}
          </ul>
        ) : c.detailFailed && !c.detail ? (
          <p className="text-label text-ink-500">{t("Agent details are unavailable.")}</p>
        ) : reading(c) ? (
          <p className="text-label text-ink-500">{t("Loading agent details…")}</p>
        ) : (
          <p className="text-label text-ink-500">
            {t("No callable capability inventory is reported for this agent yet.")}
          </p>
        )}
      </section>

      <section>
        <div className="ap-panel-h">
          <h3>{t("Connections")}</h3>
        </div>
        {notServed ? (
          <p className="text-label text-ink-500">{t("This board doesn’t record connection access.")}</p>
        ) : failure ? (
          <p className="text-label text-warn" role="alert">
            {t(failure)}{" "}
            <button type="button" className="lnk" onClick={() => setAttempt((n) => n + 1)}>
              {t("Retry")}
            </button>
          </p>
        ) : access === null ? (
          <p className="text-label text-ink-500">{t("Reading connection access…")}</p>
        ) : access.grants.length === 0 ? (
          <p className="text-label text-ink-500">
            {t("This agent holds no connection grants. Credentials stay in custody; an agent receives only authorized operations.")}
          </p>
        ) : (
          <ul className="space-y-1">
            {access.grants.map((grant) => {
              const row = access.connections.find(
                (candidate) =>
                  candidate.provider === grant.platform && candidate.account === grant.account,
              );
              const usable =
                row !== undefined &&
                row.status.adapter_registered &&
                row.status.descriptor_available &&
                row.status.custody_available;
              return (
                <li key={`${grant.platform}:${grant.account}`} className="text-label min-w-0">
                  <span className="text-ink-200 num break-words">
                    {grant.platform} · {grant.account}
                  </span>
                  <span className="text-ink-500"> — {grant.scopes.join(", ")}</span>
                  <span className={usable ? "text-ink-400" : "text-warn"}>
                    {" · "}
                    {usable
                      ? t("connection available")
                      : row === undefined
                        ? t("connection not listed")
                        : t("connection unavailable")}
                  </span>
                </li>
              );
            })}
          </ul>
        )}
        {access?.connections_error ? (
          <p className="text-micro text-warn mt-1" role="status">
            {t("Connection metadata is unavailable while protected custody is locked — the grants above are still durable.")}
          </p>
        ) : null}
        <p className="text-micro text-ink-500 mt-1">
          {t("Network access is not checked here. These rows record authority, not a provider probe.")}
        </p>
      </section>

      <section>
        <div className="ap-panel-h">
          <h3>{t("Access & security")}</h3>
        </div>
        {custody === null ? (
          <p className="text-label text-ink-500">{t("Custody state is not reported.")}</p>
        ) : (
          <dl className="text-label space-y-1">
            <div className="flex flex-wrap gap-x-2">
              <dt className="text-ink-500">{t("Encrypted store")}</dt>
              <dd className="text-ink-200">{custody.configured ? t("configured") : t("not configured")}</dd>
            </div>
            <div className="flex flex-wrap gap-x-2">
              <dt className="text-ink-500">{t("Key session")}</dt>
              <dd className="text-ink-200">
                {custody.fenced ? t("fenced") : custody.unlocked ? t("unlocked") : t("locked")}
              </dd>
            </div>
            <div className="flex flex-wrap gap-x-2">
              <dt className="text-ink-500">{t("Runtime isolation")}</dt>
              <dd className={isolated ? "text-ink-200" : "text-warn"}>
                {isolated ? custody.isolation : t("none — storage only")}
              </dd>
            </div>
            <div className="flex flex-wrap gap-x-2">
              <dt className="text-ink-500">{t("Protected execution")}</dt>
              <dd className={custody.protected && isolated ? "text-ink-200" : "text-warn"}>
                {custody.protected && isolated ? t("reported protected") : t("unavailable")}
              </dd>
            </div>
          </dl>
        )}
        <p className="text-micro text-ink-500 mt-1">
          {t("Credentials, decryption and management permits never reach this agent. Changing grants or custody is the operator's action in Settings.")}
        </p>
        {c.writable && (
          <div className="flex flex-wrap gap-2 mt-2">
            <Button size="sm" onClick={() => navigate("/settings/permissions")}>
              {t("Master permissions")}
            </Button>
            <Button size="sm" onClick={() => navigate("/settings/connections")}>
              {t("Connections")}
            </Button>
          </div>
        )}
      </section>
    </div>
  );
}

export default function AgentTabBody({ tab, c }: { tab: AgentTab; c: TabContext }) {
  switch (tab) {
    case "history":
      return <History c={c} />;
    case "skills":
      return <Skills c={c} />;
    case "tools":
      return <Tools c={c} />;
    default:
      return <Instructions c={c} />;
  }
}
