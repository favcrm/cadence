import { useEffect, useRef, useState } from "react";
import { api } from "../../lib/api";
import { ApiError } from "../../lib/apiError";
import { fmtTime } from "../../lib/fmt";
import { navigate } from "../../lib/useLocation";
import { setComposerDraft } from "../app-shell/chat/composerStore";
import { useTranslate } from "../../lib/locale";
import Button from "../../ui/Button";
import type { ToastMsg } from "../../ui/Toast";
import DrawerShell from "../app-shell/shared/DrawerShell";
import type { Agent, AgentAccess, AgentDetail, AgentRole, HiredProfile, WorkspaceSkillRead, WorkspaceSkillSummary, WorkspaceSkillUser } from "../../lib/types";
import ConfirmDialog from "../app-shell/shared/ConfirmDialog";
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
import RoutinesTab from "./RoutinesTab";
import { assignedCount, isBuiltIn, libraryAbsent, liveSkillDeps, refusal, skillFiles, statusParts, unapprovedNames, usedByNote, type SkillDeps } from "./workspaceSkills";

export interface TabContext {
  alias: string;
  profile: Agent | undefined;
  observed: Agent | undefined;
  detail: AgentDetail | null;
  role: AgentRole | null;
  hosted: boolean;
  /** The board is hosted (its meta or its install says so). */
  hostedBoard: boolean;
  resume: string | null | undefined;
  onOpenIssue: (id: string) => void;
  /** Set when this page is a profile with no runtime: nothing to read but itself. */
  hired: HiredProfile | undefined;
  /** The viewer is the operator. A member's events carry no actor, so "not recorded" is only said to the operator. */
  operator: boolean;
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
        edit ? (edit.by ? t("By {actor}").replace("{actor}", () => edit.by as string) : c.operator ? t("Who isn’t recorded") : null) : (e.task_id ?? e.job_id),
        fmtTime(e.at),
      ]
        .filter(Boolean)
        .join(" · "),
      group: historyKind(e.kind),
      facts: [
        ["Event", `#${e.seq}`],
        ["Task", e.task_id],
        ["Job", e.job_id],
        ["Changed by", edit?.by],
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
  // CAD-2419: a hosted profile also has the workspace's own skill library.
  const workspace = c.hired !== undefined && c.doc.kind === "hosted";
  const none = names.length === 0 && c.roleRead === "read" ? <Note>{t("No skills are recorded for this agent.")}</Note> : null;
  if (none && !workspace) return none;
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
      {workspace && <WorkspaceSkills doc={c.doc} writable={c.writable} toast={c.toast} absent={none} />}
      {open && (
        <ItemDrawer title={open} onClose={() => setOpen(null)}>
          <p className="ap-drawer-lead">{t(line(open))}</p>
          <Facts
            items={[
              ["Source", t(bundled.includes(open) ? "Bundled" : "Role")],
              // Only a native host is known to load every bundled skill.
              ["Loaded", t(bundled.includes(open) && !c.hostedBoard ? "In every session" : "Not recorded")],
              ["Named by its role", c.role ? t(named.includes(open) ? "Yes" : "No") : null],
            ]}
          />
        </ItemDrawer>
      )}
    </div>
  );
}

type Library = { state: "reading" } | { state: "failed" } | { state: "absent" } | { state: "ready"; skills: WorkspaceSkillSummary[] };

/** The status line of a library row, each part translated on its own. */
function skillStatus(skill: WorkspaceSkillSummary, t: (english: string) => string): string {
  return statusParts(skill)
    .map((part) => t(part.text).replace("{n}", String(part.n)))
    .join(" · ");
}

/**
 * CAD-2419: the workspace skill library on a hosted profile. Any member sees
 * it and which approved skills this profile lists; only the owner (`writable`)
 * assigns them and decides approvals. A board without the library hides it.
 */
export function WorkspaceSkills({
  doc,
  writable,
  toast,
  absent = null,
  deps = liveSkillDeps,
}: {
  doc: InstructionsDoc;
  writable: boolean;
  toast: TabContext["toast"];
  /** Shown instead when the board does not serve the library. */
  absent?: React.ReactNode;
  deps?: SkillDeps;
}) {
  const t = useTranslate();
  const [library, setLibrary] = useState<Library>({ state: "reading" });
  const [attempt, setAttempt] = useState(0);
  const [open, setOpen] = useState<string | null>(null);
  const [saving, setSaving] = useState<string | null>(null);
  const [stale, setStale] = useState<"conflict" | "unconfirmed" | null>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => {
    let live = true;
    setLibrary((now) => (now.state === "ready" ? now : { state: "reading" }));
    deps
      .list()
      .then((skills) => live && setLibrary({ state: "ready", skills }))
      .catch((error: unknown) => live && setLibrary({ state: libraryAbsent(error) ? "absent" : "failed" }));
    return () => {
      live = false;
    };
  }, [deps, attempt]);
  if (library.state === "absent") return <>{absent}</>;
  // A control that goes away when it is used hands focus to the section, not the page.
  const toHeading = () => setTimeout(() => heading.current?.focus(), 0);

  const assigned = doc.read === "ready" && doc.profile ? doc.profile.skills : null;
  const canAssign = writable && assigned !== null && stale === null;
  const skills = library.state === "ready" ? library.skills : [];
  const replace = (skill: WorkspaceSkillSummary) =>
    setLibrary((now) => (now.state === "ready" ? { state: "ready", skills: now.skills.map((s) => (s.name === skill.name ? { ...skill, used_by: skill.used_by ?? s.used_by } : s)) } : now));
  const drop = (name: string) =>
    setLibrary((now) => (now.state === "ready" ? { state: "ready", skills: now.skills.filter((s) => s.name !== name) } : now));

  const toggle = async (name: string, on: boolean) => {
    if (!assigned || saving !== null) return;
    setSaving(name);
    const out = await doc.saveSkills(on ? [...assigned, name] : assigned.filter((n) => n !== name));
    setSaving(null);
    if (out.kind === "saved") {
      toast("ok", `${t(on ? "Assigned {name}." : "Unassigned {name}.").replace("{name}", () => name)} ${t("It applies when a session next loads this profile.")}`);
    } else if (out.kind === "conflict") {
      setStale("conflict");
      toast("warn", t("Someone else changed this agent, so nothing was saved. Reload to see the current skills."));
    } else if (out.kind === "unconfirmed") {
      setStale("unconfirmed");
      toast("warn", t("Not confirmed: the connection dropped. Reload to see what the board holds."));
    } else if (out.kind === "refused") toast("err", t(out.message));
  };
  const reload = () => {
    // A reload shows the board's profile; an Instructions draft would then save over theirs.
    if (doc.draft !== null) {
      toast("warn", t("Save or discard the Instructions draft first, then reload."));
      return;
    }
    setStale(null);
    doc.retry();
    toHeading();
  };

  const usedBy = (skill: WorkspaceSkillSummary) => {
    const note = usedByNote(skill);
    return note ? t(note.text).replace("{n}", String(note.n)) : "";
  };
  const unlisted = assigned && library.state === "ready" ? unapprovedNames(assigned, skills) : [];
  const count = assigned && library.state === "ready" ? assignedCount(assigned, skills) : null;
  return (
    <section data-work-panel="workspace-skills">
      <div className="ap-panel-h">
        <h3 ref={heading} tabIndex={-1}>
          {t("Workspace skills")}
        </h3>
        {count && (
          <small className="num">
            {t("{n} of {m} assigned").replace("{n}", String(count.assigned)).replace("{m}", String(count.approved))}
          </small>
        )}
      </div>
      {library.state === "reading" ? (
        <Note>{t("Loading…")}</Note>
      ) : library.state === "failed" ? (
        <Failed
          text="Workspace skills couldn’t be read."
          onRetry={() => {
            setAttempt((n) => n + 1);
            toHeading();
          }}
        />
      ) : skills.length === 0 ? (
        <Note>{t("No workspace skills yet. Agents draft them; an owner approves them.")}</Note>
      ) : (
        <Rows>
          {skills.map((skill) => {
            const on = assigned?.includes(skill.name) ?? false;
            return (
              <div key={skill.name} className="ap-skill">
                <ManageRow
                  id={`wskill-${skill.name}`}
                  name={skill.name}
                  line={[skill.description, skillStatus(skill, t), usedBy(skill)].filter(Boolean).join(" · ")}
                  onOpen={() => setOpen(skill.name)}
                />
                {skill.approved && assigned !== null &&
                  (canAssign ? (
                    <label className="ap-skill-assign">
                      <input
                        type="checkbox"
                        checked={on}
                        // Not `disabled`: that would drop the focus while the save runs.
                        aria-disabled={saving !== null}
                        aria-label={`${t("Assigned")}: ${skill.name}`}
                        onChange={(e) => void toggle(skill.name, e.target.checked)}
                      />
                      {t("Assigned")}
                    </label>
                  ) : (
                    <small className="ap-skill-assign" data-assigned={on}>
                      {t(on ? "Assigned" : "Not assigned")}
                    </small>
                  ))}
              </div>
            );
          })}
        </Rows>
      )}
      {stale && (
        <p className="rc-note" role="alert">
          {t(stale === "conflict" ? "Someone else changed this agent’s profile." : "The last change wasn’t confirmed.")}{" "}
          <button type="button" className="lnk" onClick={reload}>
            {t("Reload")}
          </button>
        </p>
      )}
      {doc.read === "failed" && (
        <Failed
          text="This agent’s assigned skills couldn’t be read."
          onRetry={() => {
            doc.retry();
            toHeading();
          }}
        />
      )}
      {unlisted.length > 0 && (
        <div className="mt-3">
          <h4 className="slabel mb-2">{t("Listed on this agent")}</h4>
          <Rows>
            {unlisted.map((name) => (
              <Row key={name} left={name} right={t(isBuiltIn(name) ? "Built-in" : "Not approved in this workspace")} />
            ))}
          </Rows>
        </div>
      )}
      {open && (
        <SkillDrawer
          key={open}
          name={open}
          profileId={doc.profile?.id ?? null}
          writable={writable}
          deps={deps}
          toast={toast}
          onChanged={replace}
          onGone={(name) => {
            drop(name);
            setOpen(null);
            // The drawer and its opener row are gone: focus lands on the section, not the page.
            toHeading();
          }}
          onClose={() => setOpen(null)}
        />
      )}
    </section>
  );
}

/** An agent's name in "Used by": a link to its Skills tab, unless it is this agent or archived. */
function usedByAgent(user: WorkspaceSkillUser, profileId: string | null, t: (english: string) => string): React.ReactNode {
  if (user.profile_id === profileId || user.status === "archived") return user.name;
  return (
    <Link className="lnk" href={`/agents/${encodeURIComponent(user.name)}?tab=skills`} aria-label={`${t("Skills")}: ${user.name}`}>
      {user.name}
    </Link>
  );
}

function usedByMark(user: WorkspaceSkillUser, profileId: string | null, t: (english: string) => string): string | undefined {
  const marks = [user.profile_id === profileId && t("this agent"), user.status === "archived" && t("archived")].filter(Boolean);
  return marks.length ? marks.join(" · ") : undefined;
}

/** One library skill: what an agent would read, and the owner's decisions on it. */
function SkillDrawer({
  name,
  profileId,
  writable,
  deps,
  toast,
  onChanged,
  onGone,
  onClose,
}: {
  name: string;
  /** The profile whose Skills tab this is, so its own entry reads "this agent". */
  profileId: string | null;
  writable: boolean;
  deps: SkillDeps;
  toast: TabContext["toast"];
  onChanged: (skill: WorkspaceSkillSummary) => void;
  onGone: (name: string) => void;
  onClose: () => void;
}) {
  const t = useTranslate();
  const [shown, setShown] = useState<number | undefined>(undefined);
  const [attempt, setAttempt] = useState(0);
  const [read, setRead] = useState<WorkspaceSkillRead | null | "failed">(null);
  const [busy, setBusy] = useState(false);
  const [confirming, setConfirming] = useState(false);
  // Where focus goes once the next read lands: a decision swaps its button
  // (Approve for Revoke), and Retry or a revision switch replaces its own.
  const focusNext = useRef<"approve" | "revoke" | "top" | null>(null);
  const top = useRef<HTMLDivElement>(null);
  useEffect(() => {
    let live = true;
    // The last answer stays on screen until the next one lands, so the
    // focused control is not unmounted under a reload.
    deps
      .read(name, shown)
      .then((data) => {
        if (!live) return;
        setRead(data);
        onChanged(data.skill);
      })
      .catch((error: unknown) => {
        if (!live) return;
        if (refusal(error).code === "not_found") {
          toast("warn", t("This skill no longer exists."));
          onGone(name);
        } else setRead("failed");
      });
    return () => {
      live = false;
    };
    // Only what is asked starts a read; the callbacks are fresh each render.
  }, [deps, name, shown, attempt]);
  useEffect(() => {
    const want = focusNext.current;
    if (want === null || read === null || read === "failed") return;
    focusNext.current = null;
    (want === "top" ? top.current : (document.getElementById(`wskill-${want}`) ?? top.current))?.focus();
  }, [read]);

  const decide = async (
    job: () => Promise<WorkspaceSkillSummary | null>,
    done: string,
    n: number,
    focus: { done: "approve" | "revoke" | null; failed: "approve" | "revoke" | null },
  ) => {
    if (busy) return;
    setBusy(true);
    try {
      const skill = await job();
      setConfirming(false);
      toast("ok", t(done).replace("{name}", () => name).replace("{n}", String(n)));
      if (skill) {
        onChanged(skill);
        focusNext.current = focus.done;
        setAttempt((n) => n + 1);
      } else onGone(name);
    } catch (error) {
      const why = refusal(error);
      setConfirming(false);
      toast(why.unconfirmed ? "warn" : "err", t(why.text));
      if (why.code === "not_found") onGone(name);
      // The skill moved, or the answer was lost: show what the board holds now.
      else if (why.unconfirmed || why.code === "digest_mismatch" || why.code === "revision_conflict" || why.code === "approved_skill") {
        focusNext.current = focus.failed;
        setShown(undefined);
        setAttempt((n) => n + 1);
      }
    } finally {
      setBusy(false);
    }
  };

  const body = () => {
    if (read === null) return <Note>{t("Loading…")}</Note>;
    if (read === "failed") {
      return (
        <Failed
          text="This skill couldn’t be read."
          onRetry={() => {
            focusNext.current = "top";
            setAttempt((n) => n + 1);
          }}
        />
      );
    }
    const { skill, revision } = read;
    const approved = skill.approved;
    return (
      <>
        <p className="ap-drawer-lead">{revision.description}</p>
        <Facts
          items={[
            ["Status", skillStatus(skill, t)],
            ["Shown", `${t("Revision {n}").replace("{n}", String(revision.revision))}${read.approved ? ` · ${t("approved")}` : ""}`],
            ["Latest revision", String(skill.latest_revision)],
            ["Approved revision", approved ? `${approved.revision} · ${approved.digest.slice(0, 12)}` : t("None")],
            ["Author", revision.author ?? t("Not recorded")],
            ["Saved by", revision.created_by],
          ]}
        />
        {(approved && approved.revision !== revision.revision) || revision.revision !== skill.latest_revision ? (
          <div className="flex flex-wrap gap-2">
            {approved && approved.revision !== revision.revision && (
              <Button
                size="sm"
                variant="ghost"
                onClick={() => {
                  focusNext.current = "top";
                  setShown(approved.revision);
                }}
              >
                {t("Show the approved revision")}
              </Button>
            )}
            {revision.revision !== skill.latest_revision && (
              <Button
                size="sm"
                variant="ghost"
                onClick={() => {
                  focusNext.current = "top";
                  setShown(undefined);
                }}
              >
                {t("Show the latest revision")}
              </Button>
            )}
          </div>
        ) : null}
        {/* No `disabled` while a decision runs: it would drop the focus. `decide` ignores a second click. */}
        {writable && (
          <div className="flex flex-wrap gap-2" aria-busy={busy}>
            {!read.approved && (
              <Button
                size="sm"
                variant="primary"
                id="wskill-approve"
                onClick={() =>
                  void decide(() => deps.approve(name, revision.revision, revision.digest), "Approved revision {n} of {name}.", revision.revision, {
                    done: "revoke",
                    failed: "approve",
                  })
                }
              >
                {t("Approve revision {n}").replace("{n}", String(revision.revision))}
              </Button>
            )}
            {approved && (
              <Button
                size="sm"
                id="wskill-revoke"
                onClick={() => void decide(() => deps.revoke(name, approved.digest), "Revoked {name}.", 0, { done: "approve", failed: "revoke" })}
              >
                {t("Revoke")}
              </Button>
            )}
            {!approved && (
              <Button size="sm" variant="danger" onClick={() => !busy && setConfirming(true)}>
                {t("Delete")}
              </Button>
            )}
          </div>
        )}
        {skill.used_by !== undefined && (
          <div className="grid gap-1" data-work-panel="used-by">
            <h4 className="slabel">{`${t("Used by")} · ${skill.used_by.length}`}</h4>
            {skill.used_by.length === 0 ? (
              <Note>{t("No agent lists this skill yet.")}</Note>
            ) : (
              <Rows>
                {skill.used_by.map((user) => (
                  <Row key={user.profile_id} left={usedByAgent(user, profileId, t)} right={usedByMark(user, profileId, t)} />
                ))}
              </Rows>
            )}
          </div>
        )}
        <h4 className="slabel">{t("Workspace guidance, shown as plain text")}</h4>
        <Note>{t("An agent reads this as guidance from the workspace. Only an approved revision reaches an agent.")}</Note>
        {skillFiles(revision.files).map(([path, text]) => (
          <div key={path} className="grid gap-1">
            <h4 className="slabel">{path === "SKILL.md" ? `${t("Instructions")} · ${path}` : path}</h4>
            <pre className="ap-code">{text}</pre>
          </div>
        ))}
      </>
    );
  };
  const latest = read !== null && read !== "failed" ? read.skill.latest_revision : null;
  return (
    <>
      <ItemDrawer title={name} onClose={onClose}>
        <div ref={top} tabIndex={-1} className="ap-body focus:outline-none">
          {body()}
        </div>
      </ItemDrawer>
      {/* Outside the drawer: its transform would pin a fixed dialog inside it. */}
      {confirming && latest !== null && (
        <ConfirmDialog
          title={t("Delete {name}?").replace("{name}", () => name)}
          body={t("This removes the skill and every revision of it. It is not approved, so no agent reads it.")}
          confirmLabel={t("Delete")}
          pending={busy}
          error={null}
          onConfirm={() =>
            void decide(async () => {
              await deps.remove(name, latest);
              return null;
            }, "Deleted {name}.", 0, { done: null, failed: null })
          }
          onCancel={() => setConfirming(false)}
        />
      )}
    </>
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
    case "routines":
      return <RoutinesTab c={c} />;
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
