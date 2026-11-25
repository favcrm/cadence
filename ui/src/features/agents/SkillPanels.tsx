import { useId, useRef, useState, type FormEvent } from "react";
import { useTranslate } from "../../lib/localeText";
import { fmtBytes } from "../../lib/fmt";
import Button from "../../ui/Button";
import Md from "../../ui/Md";
import Link from "../../ui/Link";
import RowMenu from "../../ui/RowMenu";
import Switch from "../../ui/Switch";
import { navigate } from "../../lib/useLocation";
import { routePath } from "../../lib/router";
import type { WorkspaceSkillRead, WorkspaceSkillUser } from "../../lib/types";
import {
  SKILL_DESCRIPTION_MAX,
  SKILL_PACKAGE_BYTES_MAX,
  SKILL_REFERENCES_MAX,
  approvedWhen,
  buildDraftFiles,
  composeSkillMd,
  filterUsedBy,
  parseUploadedSkillFiles,
  referenceFiles,
  stripFrontmatter,
  usedByCounts,
  usedBySummary,
  utf8ByteLength,
  validateSkillName,
  type DraftReference,
  type UsedByFilter,
} from "./workspaceSkills";

/** CAD-2438: the panels of the skill drawer. State and writes live in SkillDrawer. */

const agentHref = (user: WorkspaceSkillUser, scopedRoster: boolean) =>
  `${routePath(scopedRoster
    ? { screen: "agents", alias: null, profileId: user.profile_id }
    : { screen: "agents", alias: user.name })}?tab=skills`;

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
  assign?: AssignView;
  profileId: string | null;
  onSeeAll: () => void;
}) {
  const t = useTranslate();
  const { skill, revision } = read;
  const approved = skill.approved;
  const users = skill.used_by;
  const summary = users ? usedBySummary(users, profileId) : null;
  const counts = users ? usedByCounts(users) : null;
  const agent = assign?.agentName ?? "";
  return (
    <div className="ap-body">
      {assign && assign.on !== null && (
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
  scopedRoster,
  notice,
  onTurnOff,
}: {
  users: WorkspaceSkillUser[];
  profileId: string | null;
  writable: boolean;
  scopedRoster: boolean;
  /** A stale or unconfirmed turn-off, with the way to read again. */
  notice: { text: string; reload: () => void } | null;
  onTurnOff?: (user: WorkspaceSkillUser) => void;
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
      {writable && onTurnOff !== undefined && users.length > 0 && (
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
            const menuItems = [
              ...(mine || archived ? [] : [{ key: "open", label: "Open agent", hint: t("Go to {agent}’s Skills tab").replace("{agent}", () => user.name), onSelect: () => navigate(agentHref(user, scopedRoster)) }]),
              ...(onTurnOff !== undefined ? [{ key: "off", label: "Turn off for this agent", hint: t("{agent} stops getting this skill").replace("{agent}", () => user.name), danger: true, onSelect: () => onTurnOff(user) }] : []),
            ];
            return (
              <li key={user.profile_id} className="sk-user">
                <span className="sk-user-name">
                  {mine || archived ? (
                    user.name
                  ) : (
                    <Link className="lnk" href={agentHref(user, scopedRoster)} aria-label={`${t("Skills")}: ${user.name}`}>
                      {user.name}
                    </Link>
                  )}
                </span>
                <small>{[mine && t("this agent"), archived && t("archived")].filter(Boolean).join(" · ")}</small>
                {writable && menuItems.length > 0 && (
                  <RowMenu
                    label={`${t("Actions for")} ${user.name}`}
                    items={menuItems}
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

/** Create or edit a draft workspace skill (`SKILL.md` + optional `references/`). */
export function SkillDraftForm({
  formId,
  mode,
  initialName = "",
  initialDescription = "",
  initialBody = "",
  initialReferences = [],
  busy,
  serverError,
  onSubmit,
}: {
  formId: string;
  mode: "create" | "edit";
  initialName?: string;
  initialDescription?: string;
  initialBody?: string;
  initialReferences?: DraftReference[];
  busy: boolean;
  serverError: string | null;
  onSubmit: (payload: { name: string; files: Record<string, string> }) => void;
}) {
  const t = useTranslate();
  const baseId = useId();
  const uploadInput = useRef<HTMLInputElement>(null);
  const [name, setName] = useState(initialName);
  const [description, setDescription] = useState(initialDescription);
  const [body, setBody] = useState(initialBody);
  const [references, setReferences] = useState<DraftReference[]>(initialReferences);
  const [bodyView, setBodyView] = useState<"edit" | "preview">("edit");
  const [localError, setLocalError] = useState<string | null>(null);

  const skillBytes = utf8ByteLength(composeSkillMd(description, body));
  const totalBytes = references.reduce((sum, ref) => sum + utf8ByteLength(ref.content), skillBytes);
  const displayedError = localError ?? serverError;

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    setLocalError(null);
    const targetName = mode === "create" ? name.trim() : initialName;
    if (mode === "create") {
      const nameError = validateSkillName(name);
      if (nameError !== null) {
        setLocalError(t(nameError));
        return;
      }
    }
    const built = buildDraftFiles({ description, body, references });
    if (!built.ok) {
      setLocalError(t(built.error));
      return;
    }
    onSubmit({ name: targetName, files: built.files });
  };

  const handleFiles = async (fileList: FileList | null) => {
    if (!fileList || fileList.length === 0) return;
    setLocalError(null);
    const uploads = await Promise.all(
      Array.from(fileList).map(async (file) => ({
        name: file.webkitRelativePath || file.name,
        content: await file.text(),
      })),
    );
    const parsed = parseUploadedSkillFiles(uploads);
    if (parsed.description !== undefined) setDescription(parsed.description);
    if (parsed.body !== undefined) setBody(parsed.body);
    if (parsed.references.length > 0) {
      setReferences((prev) => {
        const next = [...prev];
        for (const incoming of parsed.references) {
          const at = next.findIndex((item) => item.name === incoming.name);
          if (at >= 0) next[at] = incoming;
          else if (next.length < SKILL_REFERENCES_MAX) next.push(incoming);
        }
        return next;
      });
    }
    if (parsed.errors.length > 0) setLocalError(t(parsed.errors[0]!));
    if (uploadInput.current) uploadInput.current.value = "";
  };

  return (
    <form id={formId} className="sk-form" onSubmit={submit}>
      {mode === "create" && (
        <div className="sk-field">
          <label htmlFor={`${baseId}-name`}>{t("Skill name")}</label>
          <input
            id={`${baseId}-name`}
            type="text"
            className="field num"
            value={name}
            maxLength={32}
            autoComplete="off"
            spellCheck={false}
            placeholder="weekly-report"
            disabled={busy}
            onChange={(e) => {
              setName(e.target.value);
              setLocalError(null);
            }}
          />
          <small className="rc-note">{t("Lowercase letters, digits and hyphens, up to 32 characters.")}</small>
        </div>
      )}

      <div className="sk-field">
        <label htmlFor={`${baseId}-desc`}>{t("Description")}</label>
        <input
          id={`${baseId}-desc`}
          type="text"
          className="field"
          value={description}
          maxLength={SKILL_DESCRIPTION_MAX}
          autoComplete="off"
          placeholder={t("When an agent should use this skill")}
          disabled={busy}
          onChange={(e) => {
            setDescription(e.target.value);
            setLocalError(null);
          }}
        />
      </div>

      <div className="sk-field">
        <div className="sk-form-bar">
          <label htmlFor={`${baseId}-body`}>{t("Instructions (SKILL.md)")}</label>
          <Segment
            label={t("View as")}
            value={bodyView}
            options={[["edit", t("Edit")], ["preview", t("Preview")]]}
            onChange={setBodyView}
          />
        </div>
        {bodyView === "edit" ? (
          <textarea
            id={`${baseId}-body`}
            className="field sk-textarea"
            rows={10}
            value={body}
            placeholder={t("Write step-by-step guidance for the agent...")}
            disabled={busy}
            onChange={(e) => {
              setBody(e.target.value);
              setLocalError(null);
            }}
          />
        ) : (
          <div className="sk-doc">
            <div className="sk-md" data-view="preview">
              {body.trim() ? <Md text={body} /> : <p className="rc-note">{t("Nothing to preview yet.")}</p>}
            </div>
          </div>
        )}
      </div>

      <div className="sk-field">
        <div className="sk-form-bar">
          <span className="slabel">{t("Reference files")} ({references.length}/{SKILL_REFERENCES_MAX})</span>
          <div className="flex flex-wrap gap-2">
            <input
              ref={uploadInput}
              type="file"
              multiple
              accept=".md,.txt,text/markdown,text/plain"
              className="sk-upload-input"
              aria-label={t("Upload skill files")}
              disabled={busy}
              onChange={(e) => void handleFiles(e.target.files)}
            />
            <Button size="sm" disabled={busy} onClick={() => uploadInput.current?.click()}>
              {t("Upload files")}
            </Button>
            <Button
              size="sm"
              disabled={busy || references.length >= SKILL_REFERENCES_MAX}
              onClick={() => {
                setLocalError(null);
                setReferences((prev) => [...prev, { name: "", content: "" }]);
              }}
            >
              {t("Add reference")}
            </Button>
          </div>
        </div>
        {references.length === 0 ? (
          <p className="rc-note">{t("Optional plain-text or Markdown files placed under references/.")}</p>
        ) : (
          <div className="sk-ref-list">
            {references.map((ref, idx) => (
              <div key={idx} className="sk-ref-card">
                <div className="sk-ref-head">
                  <span className="num text-label text-ink-500">references/</span>
                  <input
                    type="text"
                    className="field num flex-1 min-w-0"
                    aria-label={t("Reference filename")}
                    placeholder="checklist.md"
                    value={ref.name}
                    disabled={busy}
                    onChange={(e) => {
                      const val = e.target.value;
                      setLocalError(null);
                      setReferences((prev) => prev.map((item, i) => (i === idx ? { ...item, name: val } : item)));
                    }}
                  />
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={busy}
                    onClick={() => {
                      setLocalError(null);
                      setReferences((prev) => prev.filter((_, i) => i !== idx));
                    }}
                  >
                    {t("Remove")}
                  </Button>
                </div>
                <textarea
                  rows={4}
                  className="field sk-textarea"
                  aria-label={t("Reference content")}
                  placeholder={t("Reference text...")}
                  value={ref.content}
                  disabled={busy}
                  onChange={(e) => {
                    const val = e.target.value;
                    setLocalError(null);
                    setReferences((prev) => prev.map((item, i) => (i === idx ? { ...item, content: val } : item)));
                  }}
                />
              </div>
            ))}
          </div>
        )}
      </div>

      <p className="rc-note">
        {t("Package size")}: <span className="num">{fmtBytes(totalBytes)}</span> / <span className="num">{fmtBytes(SKILL_PACKAGE_BYTES_MAX)}</span>.{" "}
        {t("Saves as a draft. An owner must approve it before agents can read it.")}
      </p>

      {displayedError && (
        <p className="text-label text-fail break-words" role="alert">
          {displayedError}
        </p>
      )}
    </form>
  );
}
