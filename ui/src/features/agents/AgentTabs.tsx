import { useContext, useState } from "react";
import { api } from "../../lib/api";
import { ApiError } from "../../lib/apiError";
import { fmtTime } from "../../lib/fmt";
import { navigate } from "../../lib/useLocation";
import { setComposerDraft } from "../app-shell/chat/composerStore";
import { useTranslate } from "../../lib/locale";
import Button from "../../ui/Button";
import { HireBlock, WriteGate } from "../auth/WriteGate";
import type { Agent, AgentDetail, AgentRole } from "../../lib/types";
import { AgentActivity, AgentQueue, WorkBlock } from "./AgentBlocks";
import { ProfileBlock } from "./AgentProfile";
import { appliesText } from "./agentRoles";
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
  fenced: boolean | undefined;
  lifecycle: string;
  deadHolding: boolean;
  onOpenIssue: (id: string) => void;
  onTab: (tab: AgentTab) => void;
  onSaved: () => void;
}

function Panel({
  title,
  link,
  onTab,
  children,
}: {
  title: string;
  link?: AgentTab;
  onTab: (tab: AgentTab) => void;
  children: React.ReactNode;
}) {
  const t = useTranslate();
  return (
    <section>
      <div className="ap-panel-h">
        <h3>{t(title)}</h3>
        {link && (
          <button type="button" className="ap-link" onClick={() => onTab(link)}>
            {t("View all")}
          </button>
        )}
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

function Chips({ items, planned = [] }: { items: string[]; planned?: string[] }) {
  const t = useTranslate();
  return (
    <div className="rc-kst-row">
      {items.map((item) => (
        <span key={item} className="rc-kst">
          {item}
        </span>
      ))}
      {planned.map((item) => (
        <span key={item} className="rc-kst planned">
          {t(item)} · {t("planned")}
        </span>
      ))}
    </div>
  );
}

function Note({ children }: { children: React.ReactNode }) {
  return <p className="rc-note">{children}</p>;
}

/** First-job chips: open the agent's thread with the text in the composer.
 *  Nothing is sent; the operator reviews it first. */
export function FirstJobs({ alias, tasks }: { alias: string; tasks: string[] }) {
  const t = useTranslate();
  const writeBlock = useContext(WriteGate);
  return (
    <div className="rc-sugs" style={{ marginTop: "0.5rem" }}>
      {tasks.map((task) => (
        <button
          key={task}
          type="button"
          className="rc-sug"
          disabled={writeBlock !== null}
          title={writeBlock ?? t("Opens the thread with this text ready to send")}
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

function NowCard({ c }: { c: TabContext }) {
  const t = useTranslate();
  const working = c.observed && (c.observed.running > 0 || (c.observed.tasks?.length ?? 0) > 0);
  if (c.observed && working) {
    return (
      <div className="ap-now">
        <WorkBlock agent={c.observed} onOpenIssue={c.onOpenIssue} />
        <div className="rc-sugs" style={{ marginTop: "0.5rem" }}>
          <Button size="sm" href={`/threads?dm=${encodeURIComponent(c.alias)}`}>
            {t("Open thread")}
          </Button>
          <Button size="sm" disabled title={t("Stopping a turn is not wired from the board yet")}>
            {t("Stop turn")}
          </Button>
        </div>
      </div>
    );
  }
  return (
    <div className="ap-now">
      <b>{t("Nothing right now")}</b>
      <small>{t("Give it a job:")}</small>
      {c.role && <FirstJobs alias={c.alias} tasks={c.role.first_tasks} />}
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

/** The job as the mockup shows it: its first paragraph, then the bullets. */
function JobSummary({ text }: { text: string }) {
  const lines = text.split("\n").map((line) => line.trimEnd());
  const bullets = lines.filter((line) => line.startsWith("- ")).map((line) => line.slice(2));
  const lead = lines.filter((line) => line.trim() !== "" && !line.startsWith("- "));
  return (
    <div className="ap-lead ap-job-sum">
      {lead.map((line) => (
        <p key={line}>{line}</p>
      ))}
      {bullets.length > 0 && (
        <ul className="rc-does">
          {bullets.map((line) => (
            <li key={line}>{line}</li>
          ))}
        </ul>
      )}
    </div>
  );
}

function JobExcerpt({ c }: { c: TabContext }) {
  const t = useTranslate();
  const text = c.detail?.agent.instructions ?? c.role?.instructions ?? null;
  return text ? (
    <JobSummary text={text} />
  ) : (
    <p className="rc-note">{t("This agent has no role text on this board.")}</p>
  );
}

function Overview({ c }: { c: TabContext }) {
  const t = useTranslate();
  const events = c.detail?.events ?? [];
  const results = resultRows(c.detail);
  const project = c.detail?.agent.turn_project ?? null;
  return (
    <div className="ap-grid">
      <div className="ap-main">
        <Panel title="Doing now" onTab={c.onTab}>
          <NowCard c={c} />
        </Panel>
        <Panel title="Up next" link="work" onTab={c.onTab}>
          {c.observed ? <AgentQueue agent={c.observed} /> : <Note>{t("Nothing queued.")}</Note>}
        </Panel>
        <Panel title="Recent results" link="work" onTab={c.onTab}>
          {results.length ? (
            <Rows>
              {results.slice(0, 3).map((e) => (
                <EventRow key={e.seq} e={e} />
              ))}
            </Rows>
          ) : (
            <Note>{t("No results yet.")}</Note>
          )}
        </Panel>
        <Panel title="History" link="history" onTab={c.onTab}>
          {events.length ? (
            <Rows>
              {events.slice(-3).reverse().map((e) => (
                <EventRow key={e.seq} e={e} />
              ))}
            </Rows>
          ) : (
            <Note>{t("No events yet.")}</Note>
          )}
        </Panel>
      </div>
      <aside className="ap-side">
        <Panel title="Job" link="job" onTab={c.onTab}>
          <JobExcerpt c={c} />
        </Panel>
        <Panel title="Knowledge" link="knowledge" onTab={c.onTab}>
          <Chips
            items={[t("Workspace wiki"), ...(project ? [`${t("Project notes")}: ${project}`] : [t("Ticket's project notes")])]}
            planned={["Its own memory"]}
          />
        </Panel>
        <Panel title="Skills" link="skills" onTab={c.onTab}>
          <Chips items={c.role?.skills ?? [...BUNDLED_SKILLS]} />
        </Panel>
        <Panel title="MCP & tools" link="tools" onTab={c.onTab}>
          <Chips items={["cadence"]} planned={["Per-agent MCP"]} />
        </Panel>
        <Panel title="Model & usage" link="settings" onTab={c.onTab}>
          {c.profile ? <ProfileBlock agent={c.profile} /> : <Note>{t("Loading agent details…")}</Note>}
        </Panel>
      </aside>
    </div>
  );
}

function Work({ c }: { c: TabContext }) {
  const t = useTranslate();
  const detail = c.detail;
  const results = resultRows(detail);
  return (
    <div className="ap-body">
      <Panel title="Doing now" onTab={c.onTab}>
        <NowCard c={c} />
        {c.observed && (
          <div className="mt-2">
            <AgentActivity agent={c.observed} />
          </div>
        )}
      </Panel>
      <Panel title="Queue" onTab={c.onTab}>
        {c.observed ? <AgentQueue agent={c.observed} /> : <Note>{t("Nothing queued.")}</Note>}
        {(detail?.tasks?.length ?? 0) > 0 && (
          <div className="mt-3">
            <h4 className="slabel mb-2">{t("Tasks")} · {t("assigned in flight")}</h4>
            <Rows>
              {detail!.tasks!.map((task) => (
                <Row key={task} left={<span className="num">{task}</span>} />
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
      <Panel title="Results" onTab={c.onTab}>
        {results.length ? (
          <Rows>
            {results.map((e) => (
              <EventRow key={e.seq} e={e} />
            ))}
          </Rows>
        ) : (
          <Note>{t("No results yet.")}</Note>
        )}
      </Panel>
      <Note>
        {t("Work = tasks dispatched to it + tickets it owns, plus filed reports.")}
      </Note>
    </div>
  );
}

function History({ c }: { c: TabContext }) {
  const t = useTranslate();
  const [filter, setFilter] = useState<HistoryFilter>("all");
  const detail = c.detail;
  const inbox = (detail?.running ?? []).map((m) => ({
    key: `m-${m.id}`,
    kind: "turn running",
    label: m.id,
    sub: m.task ? t("task {task}").replace("{task}", () => String(m.task)) : (m.summary ?? ""),
    at: m.created ? fmtTime(m.created) : "",
    group: "turns" as const,
  }));
  const events = (detail?.events ?? []).map((e) => ({
    key: `e-${e.seq}`,
    kind: e.kind,
    label: `#${e.seq} ${e.kind}`,
    sub: e.task_id ?? e.job_id ?? "",
    at: fmtTime(e.at),
    group: historyKind(e.kind),
  }));
  const rows = [...inbox, ...events].filter((r) => filter === "all" || r.group === filter);
  return (
    <div className="ap-body">
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
      {detail && (
        <p className="text-micro text-ink-500">
          {t("{running} running · {queued} queued · {unknown} unknown")
            .replace("{running}", String(detail.running.length))
            .replace("{queued}", String(detail.queued))
            .replace("{unknown}", String(detail.unknown))}
          {" · "}
          {t("last {count} events").replace("{count}", String(detail.events.length))}
        </p>
      )}
      {rows.length ? (
        <Rows>
          {rows.map((r) => (
            <Row
              key={r.key}
              left={
                <>
                  {r.label} {r.sub && <small>{r.sub}</small>}
                </>
              }
              right={r.at}
            />
          ))}
        </Rows>
      ) : (
        <Note>{t("No events yet.")}</Note>
      )}
    </div>
  );
}

function Job({ c }: { c: TabContext }) {
  const t = useTranslate();
  const writeBlock = useContext(HireBlock);
  // The board serves the saved text to the operator only; anyone else sees
  // the role's job line in the hero and no text box.
  const served = c.detail?.agent.instructions ?? null;
  const [text, setText] = useState(served ?? "");
  const [saving, setSaving] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [applies, setApplies] = useState<string | null>(null);
  const operator = writeBlock === null;
  const save = async () => {
    if (!operator || saving) return;
    setSaving(true);
    setErr(null);
    setApplies(null);
    try {
      const out = await api.setAgentInstructions(c.alias, text);
      setApplies(out.applies);
      c.onSaved();
    } catch (error) {
      setErr(error instanceof ApiError || error instanceof Error ? error.message : String(error));
    } finally {
      setSaving(false);
    }
  };
  if (!c.detail) return <Note>{t("Loading agent details…")}</Note>;
  if (served === null) {
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
  return (
    <div className="ap-body">
      <label className="rc-fld">
        <span className="lab">{t("Standing instructions")}</span>
        <textarea
          className="field rc-text"
          style={{ minHeight: "17.5rem" }}
          value={text}
          readOnly={!operator}
          onChange={(event) => setText(event.target.value)}
        />
        <span className="hint">
          {c.role
            ? t("Started from the {role} role. Edits change only this agent and apply from its next turn.").replace(
                "{role}",
                () => c.role!.title,
              )
            : t("Edits change only this agent.")}
        </span>
      </label>
      {operator ? (
        <div className="rc-sugs">
          <Button variant="primary" size="sm" disabled={saving} onClick={() => void save()}>
            {saving ? t("Saving…") : t("Save")}
          </Button>
          {c.role && (
            <Button size="sm" onClick={() => setText(c.role!.instructions)}>
              {t("Reset to role")}
            </Button>
          )}
        </div>
      ) : (
        <Note>{writeBlock}</Note>
      )}
      {applies && (
        <p className="ap-saved" role="status">
          {t("Saved.")} {t(appliesText(applies))}
        </p>
      )}
      {err && (
        <p role="alert" className="text-label text-fail">
          {err}
        </p>
      )}
    </div>
  );
}

function Knowledge({ c }: { c: TabContext }) {
  const t = useTranslate();
  const project = c.detail?.agent.turn_project ?? null;
  return (
    <div className="ap-body">
      <Panel title="Its memory" onTab={c.onTab}>
        <Note>
          {t(
            "Planned: per-agent memory. Today memory is per project, proposed by agents and approved by two reviewers.",
          )}
        </Note>
      </Panel>
      <Panel title="Reads" onTab={c.onTab}>
        <Rows>
          <Row left={t("Workspace wiki")} right="global/" />
          <Row
            left={t("Project notes of the ticket it works on")}
            right={
              project
                ? `projects/${project}/`
                : t("projects/<key>/ — only while on that ticket")
            }
          />
          <Row left={t("Files you attach in a thread")} right={t("per conversation")} />
        </Rows>
      </Panel>
      <Panel title="Its notes" onTab={c.onTab}>
        <Rows>
          <div className="ap-row">
            <span className="num">{`agents/${c.alias}/knowledge`}</span>
            <Button
              variant="ghost"
              size="sm"
              href={`/wiki/agents/${encodeURIComponent(c.alias)}/knowledge`}
            >
              {t("Open")}
            </Button>
          </div>
        </Rows>
      </Panel>
    </div>
  );
}

function Skills({ c }: { c: TabContext }) {
  const t = useTranslate();
  return (
    <div className="ap-body">
      <Panel title="Skills" onTab={c.onTab}>
        <Rows>
          {BUNDLED_SKILLS.map((skill) => {
            const on = c.role ? c.role.skills.includes(skill) : true;
            return (
              <div key={skill} className="ap-row">
                <span>
                  <b className="rt">{skill}</b> <span className="kind">{t("bundled")}</span>
                </span>
                <span className={`ap-pill ${on ? "on" : "off"}`}>{on ? t("on") : t("off")}</span>
              </div>
            );
          })}
        </Rows>
      </Panel>
      <Note>
        {t(
          "Today every agent on the machine loads all four bundled skills. On/off per agent is planned (per-home skills dir or Pi --skill).",
        )}
      </Note>
    </div>
  );
}

function Tools({ c }: { c: TabContext }) {
  const t = useTranslate();
  return (
    <div className="ap-body">
      <Panel title="MCP servers & connections" onTab={c.onTab}>
        <Rows>
          <div className="ap-row">
            <span>
              <b className="rt">cadence</b> <span className="kind">{t("built in")}</span>
              <br />
              <small>{t("Tickets, threads, reports")}</small>
            </span>
            <span className="ap-pill on">{t("on")}</span>
          </div>
          {(c.role?.tools ?? []).map((tool) => (
            <div key={tool} className="ap-row">
              <span>{tool}</span>
              <small className="pl">{t("planned")}</small>
            </div>
          ))}
        </Rows>
        <div className="rc-sugs" style={{ marginTop: "0.65rem" }}>
          <Button size="sm" disabled title={t("Per-agent MCP is planned (CAD-1754)")}>
            ＋ {t("Add MCP server")}
          </Button>
          <Button size="sm" disabled title={t("Per-agent grants are planned (CAD-1754)")}>
            ＋ {t("Connect an app")}
          </Button>
        </div>
      </Panel>
      <Panel title="Rules" onTab={c.onTab}>
        <Rows>
          <div className="ap-row">
            <span>{t("Asks you before sending or publishing")}</span>
            <small className="pl">{t("planned")}</small>
          </div>
          <div className="ap-row">
            <span>
              {c.role?.kind === "apps"
                ? t("Read-only on app data")
                : t("Works only in the ticket's worktree")}
            </span>
            <small>{t("enforced by confinement")}</small>
          </div>
        </Rows>
      </Panel>
      <Note>
        {t(
          "Today: no per-agent MCP config; Claude --allow and platform grants are CLI-only. Logins and keys never come with a role.",
        )}
      </Note>
    </div>
  );
}

function Settings({ c }: { c: TabContext }) {
  const t = useTranslate();
  const a = c.detail?.agent;
  const writeBlock = useContext(WriteGate);
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
  const profile = c.profile;
  const lifecycle = c.lifecycle;
  void writeBlock;
  return (
    <div className="ap-body">
      <Panel title="Model, effort & usage" onTab={c.onTab}>
        {profile ? <ProfileBlock agent={profile} /> : <Note>{t("Loading agent details…")}</Note>}
      </Panel>
      <Panel title="Runtime" onTab={c.onTab}>
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
            <dt>{t("catalogue")}</dt>
            <dd>{typeof a.params?.catalogue === "string" ? a.params.catalogue : "—"}</dd>
            {identity
              .filter(([, v]) => shown(v))
              .map(([k, v]) => (
                <div key={k} className="contents">
                  <dt>{k}</dt>
                  <dd title={String(v)}>{String(v)}</dd>
                </div>
              ))}
            {Object.entries(a.params ?? {}).map(([k, v]) => (
              <div key={k} className="contents">
                <dt title={`param ${k}`}>{k}</dt>
                <dd title={typeof v === "string" ? v : JSON.stringify(v)}>
                  {typeof v === "string" ? v : JSON.stringify(v)}
                </dd>
              </div>
            ))}
          </dl>
        ) : (
          <Note>{t("Loading agent details…")}</Note>
        )}
      </Panel>
      {c.resume && (
        <Panel title="Resume command" onTab={c.onTab}>
          <p className="text-label text-ink-400 mb-2">
            {c.observed?.resume_hint ?? t("For use in a terminal by the operator.")}
          </p>
          <pre className="ap-code">{c.resume}</pre>
        </Panel>
      )}
      <Panel title="Lifecycle" onTab={c.onTab}>
        <div className="rc-sugs">
          {(c.fenced || c.deadHolding || lifecycle === "idle-holding") && (
            <>
              <Button size="sm" disabled title={t("Resume is not wired from the board yet")}>
                {t("Resume")}
              </Button>
              <Button size="sm" variant="danger" disabled title={t("Claim release is not wired from the board yet")}>
                {t("Release claim")}
              </Button>
            </>
          )}
          {(c.fenced || c.deadHolding) && (
            <>
              <Button size="sm" disabled title={t("Reassignment is not wired from the board yet")}>
                {t("Reassign")}
              </Button>
              <Button
                size="sm"
                variant="danger"
                disabled
                title={t("Unfence runs from the To do rail, where the operator records the outcome")}
              >
                {t("Unfence")}
              </Button>
            </>
          )}
          {(lifecycle === "stale-inbox" || (profile && profile.queued > 0)) && (
            <>
              <Button size="sm" disabled title={t("Inbox draining is not wired from the board yet")}>
                {t("Drain inbox")}
              </Button>
              <Button size="sm" variant="danger" disabled title={t("Archiving is not wired from the board yet")}>
                {t("Archive")}
              </Button>
            </>
          )}
          {lifecycle === "active" && (
            <Button size="sm" variant="danger" disabled title={t("Stopping a turn is not wired from the board yet")}>
              {t("Stop")}
            </Button>
          )}
          <Button size="sm" disabled title={t("Pause is not wired from the board yet")}>
            {t("Pause")}
          </Button>
          <Button size="sm" disabled title={t("Restart is not wired from the board yet")}>
            {t("Restart")}
          </Button>
          <Button size="sm" variant="danger" disabled title={t("Retiring is not wired from the board yet")}>
            {t("Retire")}
          </Button>
        </div>
        <Note>{t("read-only in this stage")}</Note>
      </Panel>
    </div>
  );
}

export default function AgentTabBody({ tab, c }: { tab: AgentTab; c: TabContext }) {
  switch (tab) {
    case "work":
      return <Work c={c} />;
    case "history":
      return <History c={c} />;
    case "job":
      return <Job c={c} />;
    case "knowledge":
      return <Knowledge c={c} />;
    case "skills":
      return <Skills c={c} />;
    case "tools":
      return <Tools c={c} />;
    case "settings":
      return <Settings c={c} />;
    default:
      return <Overview c={c} />;
  }
}
