import { useState } from "react";
import { useTranslate } from "../../lib/localeText";
import { fmtBytes } from "../../lib/fmt";
import Md from "../../ui/Md";
import Link from "../../ui/Link";
import RowMenu from "../../ui/RowMenu";
import Switch from "../../ui/Switch";
import { navigate } from "../../lib/useLocation";
import type { WorkspaceSkillRead, WorkspaceSkillUser } from "../../lib/types";
import {
  approvedWhen,
  filterUsedBy,
  referenceFiles,
  stripFrontmatter,
  usedByCounts,
  usedBySummary,
  type UsedByFilter,
} from "./workspaceSkills";

/** CAD-2438: the panels of the skill drawer. State and writes live in SkillDrawer. */

const agentHref = (name: string) => `/agents/${encodeURIComponent(name)}?tab=skills`;

/** One button of a segmented switch; `aria-pressed` marks the shown one. */
function Segment<T extends string>({ label, value, options, onChange }: { label: string; value: T; options: [T, string][]; onChange: (next: T) => void }) {
  return (
    <div className="sk-seg" role="group" aria-label={label}>
      {options.map(([id, text]) => (
        <button key={id} type="button" aria-pressed={value === id} onClick={() => onChange(id)}>
          {text}
        </button>
      ))}
    </div>
  );
}

function Detail({ label, value }: { label: string; value: string | null }) {
  const t = useTranslate();
  return value === null ? null : (
    <div>
      <dt>{t(label)}</dt>
      <dd>{value}</dd>
    </div>
  );
}

export interface AssignView {
  agentName: string;
  /** A draft cannot be turned on. */
  draft: boolean;
  /** Null: this agent's skills could not be read, so there is nothing to show. */
  on: boolean | null;
  /** The owner may flip it. */
  canChange: boolean;
  pending: boolean;
  onToggle: () => void;
}

export function OverviewPanel({
  read,
  assign,
  profileId,
  onSeeAll,
}: {
  read: WorkspaceSkillRead;
  assign: AssignView;
  profileId: string | null;
  onSeeAll: () => void;
}) {
  const t = useTranslate();
  const { skill, revision } = read;
  const approved = skill.approved;
  const users = skill.used_by;
  const summary = users ? usedBySummary(users, profileId) : null;
  const counts = users ? usedByCounts(users) : null;
  const agent = assign.agentName;
  return (
    <div className="ap-body">
      {assign.on !== null && (
        <div className="sk-assign">
          <div>
            <b>{t("On for {agent}").replace("{agent}", () => agent)}</b>
            <p>
              {assign.draft
                ? t("Approve a revision before turning it on.")
                : assign.on
                  ? t("This agent sees the skill in its brief and can read it.")
                  : t("Off: this agent can’t see or read it.")}
            </p>
          </div>
          {assign.draft ? (
            <span className="sk-state">{t("Approve first")}</span>
          ) : assign.canChange ? (
            <Switch
              checked={assign.on}
              pending={assign.pending}
              label={t("On for {agent}").replace("{agent}", () => agent)}
              onToggle={assign.onToggle}
            />
          ) : (
            <span className="sk-state">{t(assign.on ? "On" : "Off")}</span>
          )}
        </div>
      )}
      {summary && counts && (
        <section className="grid gap-1" data-work-panel="used-by-summary">
          <h4 className="slabel">{`${t("Used by")} · ${counts.active} ${t("active")}`}</h4>
          {counts.active === 0 ? (
            <p className="rc-note">{t("No agent lists this skill yet.")}</p>
          ) : (
            <p className="sk-names">
              {summary.shown.map((user) => (
                <span key={user.profile_id} className="chip bg-ink-800 text-ink-300">
                  {user.name}
                </span>
              ))}
              {summary.more > 0 && <span className="sk-more">{t("and {n} more").replace("{n}", String(summary.more))}</span>}
              <button type="button" className="lnk" onClick={onSeeAll}>
                {/* Counts what the Used by tab shows first (its Active filter), so the number matches. */}
                {t("See all {n}").replace("{n}", String(counts.active))}
              </button>
            </p>
          )}
        </section>
      )}
      <section className="grid gap-1">
        <h4 className="slabel">{t("Details")}</h4>
        <dl className="sk-details">
          <Detail label="Approved revision" value={approved ? `${approved.revision} · ${approved.digest.slice(0, 12)}` : t("None")} />
          <Detail label="Latest revision" value={String(skill.latest_revision)} />
          <Detail label="Author" value={revision.author ?? t("Not recorded")} />
          {/* The approver is shown as a date, not a raw member id; `at` is epoch milliseconds. */}
          <Detail label="Approved" value={approvedWhen(approved?.at)} />
        </dl>
      </section>
      <p className="rc-note">{t("An agent reads this as guidance from the workspace. Only an approved revision reaches an agent.")}</p>
    </div>
  );
}

export function InstructionsPanel({ read }: { read: WorkspaceSkillRead }) {
  const t = useTranslate();
  const [view, setView] = useState<"preview" | "source">("preview");
  const text = read.revision.files["SKILL.md"];
  const { revision } = read;
  return (
    <div className="ap-body">
      <h4 className="slabel">{t("Workspace guidance")}</h4>
      {text === undefined ? (
        <p className="rc-note">{t("This revision has no SKILL.md.")}</p>
      ) : (
        <div className="sk-doc">
          <div className="sk-doc-bar">
            <span className="sk-file">
              <b className="num">SKILL.md</b>
              <small>{t(read.approved ? "Revision {n} · approved" : "Revision {n} · draft").replace("{n}", String(revision.revision))}</small>
            </span>
            <Segment
              label={t("View as")}
              value={view}
              options={[["preview", t("Preview")], ["source", t("Source")]]}
              onChange={setView}
            />
          </div>
          {/* One view at a time: the other is not rendered, so no style can show both. */}
          {view === "preview" ? (
            <div className="sk-md" data-view="preview">
              <Md text={stripFrontmatter(text)} />
            </div>
          ) : (
            <pre className="ap-code" data-view="source">{text}</pre>
          )}
        </div>
      )}
    </div>
  );
}

export function UsedByPanel({
  users,
  profileId,
  writable,
  notice,
  onTurnOff,
}: {
  users: WorkspaceSkillUser[];
  profileId: string | null;
  writable: boolean;
  /** A stale or unconfirmed turn-off, with the way to read again. */
  notice: { text: string; reload: () => void } | null;
  onTurnOff: (user: WorkspaceSkillUser) => void;
}) {
  const t = useTranslate();
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<UsedByFilter>("active");
  const counts = usedByCounts(users);
  const shown = filterUsedBy(users, { query, filter, profileId });
  return (
    <div className="ap-body" data-work-panel="used-by">
      <div className="sk-tools">
        <input
          className="field sk-search"
          type="search"
          value={query}
          placeholder={t("Find an agent")}
          aria-label={t("Find an agent")}
          onChange={(e) => setQuery(e.target.value)}
        />
        <Segment
          label={t("Show")}
          value={filter}
          options={[["active", t("Active")], ["all", t("All")], ["archived", t("Archived")]]}
          onChange={setFilter}
        />
      </div>
      <p className="rc-note" role="status">
        {`${counts.active} ${t("active")}, ${counts.archived} ${t("archived")}`}
        {query.trim() !== "" && ` · ${t("{n} match").replace("{n}", String(shown.length))}`}
      </p>
      {writable && users.length > 0 && (
        <p className="rc-note">{t("Use ⋯ on a row to turn the skill off for one agent. Revoke withdraws it from every agent.")}</p>
      )}
      {notice && (
        <p className="rc-note" role="alert">
          {notice.text}{" "}
          <button type="button" className="lnk" onClick={notice.reload}>
            {t("Reload")}
          </button>
        </p>
      )}
      {shown.length === 0 ? (
        <p className="rc-note">{t(users.length === 0 ? "No agent lists this skill yet." : "No agent matches.")}</p>
      ) : (
        <ul className="sk-users">
          {shown.map((user) => {
            const mine = user.profile_id === profileId;
            const archived = user.status === "archived";
            return (
              <li key={user.profile_id} className="sk-user">
                <span className="sk-user-name">
                  {mine || archived ? (
                    user.name
                  ) : (
                    <Link className="lnk" href={agentHref(user.name)} aria-label={`${t("Skills")}: ${user.name}`}>
                      {user.name}
                    </Link>
                  )}
                </span>
                <small>{[mine && t("this agent"), archived && t("archived")].filter(Boolean).join(" · ")}</small>
                {writable && (
                  <RowMenu
                    label={`${t("Actions for")} ${user.name}`}
                    items={[
                      ...(mine || archived ? [] : [{ key: "open", label: "Open agent", hint: t("Go to {agent}’s Skills tab").replace("{agent}", () => user.name), onSelect: () => navigate(agentHref(user.name)) }]),
                      { key: "off", label: "Turn off for this agent", hint: t("{agent} stops getting this skill").replace("{agent}", () => user.name), danger: true, onSelect: () => onTurnOff(user) },
                    ]}
                  />
                )}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

export function FilesPanel({ files }: { files: Record<string, string> }) {
  const t = useTranslate();
  const refs = referenceFiles(files);
  if (refs.length === 0) return <p className="rc-note">{t("This revision has only SKILL.md.")}</p>;
  return (
    <div className="ap-body">
      {refs.map(([path, text], i) => (
        <details key={path} className="sk-file-card" open={i === 0}>
          <summary>
            <b className="num">{path}</b>
            <small>{path === "skill.json" ? t("generated") : fmtBytes(new TextEncoder().encode(text).length)}</small>
          </summary>
          {path.endsWith(".md") ? (
            <div className="sk-md">
              <Md text={stripFrontmatter(text)} />
            </div>
          ) : (
            <pre className="ap-code">{text}</pre>
          )}
        </details>
      ))}
    </div>
  );
}
