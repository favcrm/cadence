import { useEffect, useId, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { showProjectChoices } from "../../lib/router";
import type { MemoryCard, MemoryDetail } from "../../lib/types";
import { navigate } from "../../lib/useLocation";
import Button from "../../ui/Button";
import { IconChevron, IconRefresh, IconSearch } from "../../ui/icons";
import Md from "../../ui/Md";
import Select from "../../ui/Select";
import { evidenceSuffix, quorumLabel, quorumView } from "./memoryView";
import "./memory.css";
import { useTranslate } from "../../lib/locale";

const STATUSES = ["proposed", "quarantined", "accepted", "pending-revalidation", "invalidated", "rejected", "superseded"];
const TYPES = ["rule", "gotcha", "decision", "recipe"];
const label = (value: string) => value.charAt(0).toUpperCase() + value.slice(1).replaceAll("-", " ");
const options = (values: string[], all: string) => [
  { value: "", label: all },
  ...values.map((value) => ({ value, label: label(value) })),
];
const errText = (e: unknown) => (e instanceof ApiError ? e.message : String(e));

function scopeLine(m: MemoryCard, t: (english: string) => string): string {
  const bits: string[] = [];
  if (m.scope.project) bits.push(t("Project-wide"));
  if (m.scope.components.length)
    bits.push(t("Components: {values}").replace("{values}", () => m.scope.components.join(", ")));
  if (m.scope.paths.length) bits.push(t("Paths: {values}").replace("{values}", () => m.scope.paths.join(", ")));
  if (m.scope.providers.length)
    bits.push(t("Providers: {values}").replace("{values}", () => m.scope.providers.join(", ")));
  if (m.scope.tags.length) bits.push(t("Tags: {values}").replace("{values}", () => m.scope.tags.join(", ")));
  if (m.policy?.roles.length) bits.push(t("Roles: {values}").replace("{values}", () => m.policy!.roles.join(", ")));
  return bits.join(" · ") || t("No scope recorded");
}

// Count line for the lesson list: each shape is a whole sentence key so zh-TW word order stays intact.
function lessonCountLine(count: number, incomplete: boolean, query: string, t: (english: string) => string): string {
  const one = count === 1;
  let line: string;
  if (query) {
    line = incomplete
      ? (one ? t("{count} readable lesson matching “{query}” · results incomplete") : t("{count} readable lessons matching “{query}” · results incomplete"))
      : (one ? t("{count} lesson matching “{query}”") : t("{count} lessons matching “{query}”"));
  } else {
    line = incomplete
      ? (one ? t("{count} readable lesson · results incomplete") : t("{count} readable lessons · results incomplete"))
      : (one ? t("{count} lesson") : t("{count} lessons"));
  }
  return line.replace("{count}", String(count)).replace("{query}", () => query);
}

function matches(m: MemoryCard, query: string): boolean {
  const text = [
    m.fact,
    m.slug,
    m.project,
    ...m.scope.components,
    ...m.scope.paths,
    ...m.scope.providers,
    ...m.scope.tags,
    ...(m.policy?.roles ?? []),
  ]
    .join(" ")
    .toLowerCase();
  return text.includes(query.trim().toLowerCase());
}

function createdLabel(created: string, t: (english: string) => string): string {
  const date = new Date(created);
  return Number.isNaN(date.getTime())
    ? t("Not recorded")
    : date.toLocaleDateString(undefined, {
        year: "numeric",
        month: "short",
        day: "numeric",
        timeZone: "UTC",
      });
}

/** Each open selection owns its request lifetime, including reopening the same key. */
function LessonDetail({ memory }: { memory: MemoryCard }) {
  const t = useTranslate();
  const [detail, setDetail] = useState<MemoryDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);
  useEffect(() => {
    let cancelled = false;
    setDetail(null);
    setError(null);
    api
      .memory(memory.project, memory.slug)
      .then((data) => {
        if (!cancelled) setDetail(data);
      })
      .catch((e) => {
        if (!cancelled) setError(errText(e));
      });
    return () => {
      cancelled = true;
    };
  }, [memory.project, memory.slug, retry]);

  if (error)
    return (
      <div className="memory-detail-state" role="alert">
        <p>
          {t("Couldn’t load this lesson.")} <span>{error}</span>
        </p>
        <Button onClick={() => setRetry((value) => value + 1)}>{t("Retry lesson")}</Button>
      </div>
    );
  if (!detail)
    return (
      <p className="memory-detail-state" role="status">{t("Loading lesson…")}</p>
    );
  const q = quorumView(detail);
  return (
    <div className="memory-detail-content">
      <div className="memory-reading">
        <div className="issue-reader">
          <Md text={detail.body} />
        </div>
        <div className="memory-applicability">
          <h3>{t("Applies to")}</h3>
          <p>{scopeLine(detail, t)}</p>
          {detail.supersedes && <p>{t("Supersedes {supersedes}").replace("{supersedes}", () => String(detail.supersedes ?? ""))}</p>}
        </div>
      </div>
      <aside
        className="memory-evidence"
        aria-label={t("Lesson review and evidence")}
      >
        <h3>
          {q.mode === "acceptance" ? t("Review status") : t("Agent availability")}
        </h3>
        <p
          className="memory-availability"
          data-state={
            q.eligible === null
              ? "unknown"
              : q.eligible
                ? "available"
                : "blocked"
          }
        >
          {label(quorumLabel(q))}
        </p>
        <p>{q.reason}</p>
        {evidenceSuffix(detail, q) && (
          <p className="memory-freshness">{detail.evidence!.label}</p>
        )}
        <p className="memory-readonly">{t("Read-only here. Native policy determines which independent reviews or operator approval are required.")}</p>
        <details className="memory-provenance">
          <summary>
            {t("Review evidence")} <IconChevron />
          </summary>
          <dl>
            {detail.policy && (
              <>
                <div>
                  <dt>{t("Policy")}</dt>
                  <dd>
                    v{detail.policy.revision} · {label(detail.policy.nature)} · {label(detail.policy.provenance)}
                    {detail.policy.provenance === "agent-verified" && <p>{t("Source provenance is verified; this is not a passing logic check.")}</p>}
                  </dd>
                </div>
                <div>
                  <dt>{t("Audience")}</dt>
                  <dd>
                    {label(detail.policy.audience)}{detail.policy.roles.length ? ` · ${detail.policy.roles.join(", ")}` : ""}
                    {detail.policy.audience === "company" && <p>{t("Company approval level; visibility stays within this project.")}</p>}
                  </dd>
                </div>
                <div>
                  <dt>{t("Requirements")}</dt>
                  <dd><ul>{detail.policy.requirements.map((requirement) => <li key={requirement}>{requirement}</li>)}</ul></dd>
                </div>
                <div>
                  <dt>{t("Author model vendor")}</dt>
                  <dd>{detail.policy.author_vendor || t("Not confirmed")}</dd>
                </div>
                <div>
                  <dt>{t("Immutable evidence references")}</dt>
                  <dd className="num">{detail.policy.evidence_refs_count}</dd>
                </div>
                <div>
                  <dt>{t("Usage reports")}</dt>
                  <dd>
                    {t("Used {used} · wrong {wrong} · reread {reread}")
                      .replace("{used}", String(detail.policy.feedback.used_reports))
                      .replace("{wrong}", String(detail.policy.feedback.wrong_reports))
                      .replace("{reread}", String(detail.policy.feedback.reread_reports))}
                    <p>{t("Authenticated reports, not proven benefit or unique uses.")}</p>
                  </dd>
                </div>
                {detail.finalized_operations?.[0] && (
                  <div>
                    <dt>{t("Latest policy decision")}</dt>
                    <dd>{label(detail.finalized_operations[0].operation)} · {createdLabel(detail.finalized_operations[0].finalized_at, t)}</dd>
                  </div>
                )}
                {detail.policy.feedback.last_used_at && (
                  <div>
                    <dt>{t("Last reported use")}</dt>
                    <dd>{createdLabel(detail.policy.feedback.last_used_at, t)}</dd>
                  </div>
                )}
              </>
            )}
            <div>
              <dt>{t("Lesson ID")}</dt>
              <dd className="num">{detail.slug}</dd>
            </div>
            <div>
              <dt>{t("Created")}</dt>
              <dd>{createdLabel(detail.created, t)}</dd>
            </div>
            <div>
              <dt>{t("Confidence")}</dt>
              <dd>{label(detail.confidence)}</dd>
            </div>
            <div>
              <dt>{t("Author")}</dt>
              <dd>{detail.author || t("Not recorded")}</dd>
            </div>
            <div>
              <dt>{t("Source")}</dt>
              <dd>{detail.source || t("Not recorded")}</dd>
            </div>
            <div>
              <dt>{t("File")}</dt>
              <dd className="num">{detail.path}</dd>
            </div>
            <div>
              <dt>{t("Revision digest")}</dt>
              <dd className="num">{detail.revision_digest || t("Unavailable")}</dd>
            </div>
            {typeof detail.review_count === "number" && (
              <div>
                <dt>{t("Review receipts")}</dt>
                <dd>{detail.review_count}</dd>
              </div>
            )}
            {typeof detail.finalization_count === "number" && (
              <div>
                <dt>{t("Policy decisions")}</dt>
                <dd>{detail.finalization_count}</dd>
              </div>
            )}
          </dl>
        </details>
      </aside>
    </div>
  );
}

function LessonRow({
  memory,
  open,
  onToggle,
}: {
  memory: MemoryCard;
  open: boolean;
  onToggle: () => void;
}) {
  const t = useTranslate();
  const id = useId();
  const q = quorumView(memory);
  return (
    <article className="memory-row" data-open={open || undefined}>
      <button
        className="memory-row-trigger"
        type="button"
        aria-expanded={open}
        aria-controls={id}
        onClick={onToggle}
      >
        <div className="memory-row-main">
          <p className="memory-fact">{memory.fact || memory.slug}</p>
          <div className="memory-row-meta">
            <span>{memory.project}</span>
            <span>{label(memory.type)}</span>
            <span>{label(memory.status)}</span>
          </div>
          <p className="memory-scope">{scopeLine(memory, t)}</p>
        </div>
        <div className="memory-row-end">
          <span
            className="memory-availability"
            data-state={
              q.eligible === null
                ? "unknown"
                : q.eligible
                  ? "available"
                  : "blocked"
            }
          >
            {label(quorumLabel(q))}
          </span>
          <IconChevron size={14} className="memory-row-chevron" />
        </div>
      </button>
      <div id={id} hidden={!open} className="memory-detail">
        {open && <LessonDetail memory={memory} />}
      </div>
    </article>
  );
}

export default function Memory({
  project,
  projects,
  projectHref,
  onError,
}: {
  project: string;
  projects: { key: string }[];
  projectHref: (key: string) => string;
  onError: (e: unknown, verb: string) => void;
}) {
  const t = useTranslate();
  const [mems, setMems] = useState<MemoryCard[] | null>(null);
  const [loadErrs, setLoadErrs] = useState<string[]>([]);
  const [listErr, setListErr] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  const [status, setStatus] = useState("");
  const [kind, setKind] = useState("");
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState<string | null>(null);
  const searchId = useId();
  const projectId = useId();
  const statusId = useId();
  const typeId = useId();

  useEffect(() => {
    let cancelled = false;
    setMems(null);
    setListErr(null);
    setLoadErrs([]);
    setOpen(null);
    api
      .memories({
        project: project === "all" ? undefined : project,
        status: status || undefined,
        type: kind || undefined,
      })
      .then((result) => {
        if (cancelled) return;
        setMems(result.memories);
        setLoadErrs(result.memory_errors ?? []);
      })
      .catch((e) => {
        if (cancelled) return;
        setListErr(errText(e));
        setMems([]);
        onError(e, "memory list");
      });
    return () => {
      cancelled = true;
    };
  }, [project, status, kind, onError, tick]);

  const visible = (mems ?? []).filter((memory) => matches(memory, query));
  const filtered = !!(status || kind || query.trim());
  const reset = () => {
    setQuery("");
    setStatus("");
    setKind("");
    setOpen(null);
  };
  const projectOptions = [
    { value: "all", label: t("All projects") },
    ...projects.map((p) => ({ value: p.key, label: p.key })),
    ...(project !== "all" && !projects.some((p) => p.key === project)
      ? [{ value: project, label: project }]
      : []),
  ];

  return (
    <section className="memory-workspace" aria-labelledby="memory-title">
      <header className="memory-heading">
        <div>
          <h1 id="memory-title">{t("Memory")}</h1>
          <p>{t("Project lessons that help agents apply what the team has learned.")}</p>
        </div>
        <Button
          icon={<IconRefresh />}
          loading={mems === null}
          onClick={() => setTick((value) => value + 1)}
        >{t("Refresh lessons")}</Button>
      </header>
      <div className="memory-filters" role="search" aria-label={t("Find lessons")}>
        <div className="memory-filter memory-search">
          <label htmlFor={searchId}>{t("Search lessons")}</label>
          <div className="memory-search-field">
            <IconSearch />
            <input
              id={searchId}
              type="search"
              className="field"
              placeholder={t("Search claims, IDs or scope…")}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
          </div>
        </div>
        {showProjectChoices(projects.length, project) && (
          <div className="memory-filter">
            <label htmlFor={projectId}>{t("Project")}</label>
            <Select
              id={projectId}
              full
              value={project}
              options={projectOptions}
              onChange={(value) =>
                navigate(projectHref(value), { replace: true })
              }
            />
          </div>
        )}
        <div className="memory-filter">
          <label htmlFor={statusId}>{t("Status")}</label>
          <Select
            id={statusId}
            full
            value={status}
            options={options(STATUSES, t("All statuses"))}
            onChange={setStatus}
          />
        </div>
        <div className="memory-filter">
          <label htmlFor={typeId}>{t("Type")}</label>
          <Select
            id={typeId}
            full
            value={kind}
            options={options(TYPES, t("All types"))}
            onChange={setKind}
          />
        </div>
      </div>
      <div className="memory-results">
        <p role="status">
          {mems === null ? t("Loading lessons…") : listErr ? t("Lessons unavailable") : lessonCountLine(visible.length, loadErrs.length > 0, query.trim(), t)}
        </p>
        {filtered && (
          <Button variant="ghost" onClick={reset}>{t("Clear filters")}</Button>
        )}
      </div>
      {loadErrs.length > 0 && (
        <div className="memory-load-warning" role="status">
          <p>
            {loadErrs.length === 1 ? t("{length} memory file couldn’t be loaded. Results may be incomplete.").replace("{length}", String(loadErrs.length)) : t("{length} memory files couldn’t be loaded. Results may be incomplete.").replace("{length}", String(loadErrs.length))}
          </p>
          <details>
            <summary>{t("Show file errors")}</summary>
            <ul>
              {loadErrs.map((error, i) => (
                <li key={i}>{error}</li>
              ))}
            </ul>
          </details>
        </div>
      )}
      {listErr ? (
        <div className="memory-state" role="alert">
          <h2>{t("Couldn’t load lessons")}</h2>
          <p>{listErr}</p>
          <Button onClick={() => setTick((value) => value + 1)}>{t("Retry lessons")}</Button>
        </div>
      ) : mems === null ? (
        <div className="memory-state" aria-busy="true">
          <p>{t("Loading project memory…")}</p>
        </div>
      ) : visible.length === 0 && loadErrs.length > 0 ? (
        <div className="memory-state">
          <h2>{filtered ? t("No readable matches") : t("No readable lessons")}</h2>
          <p>{t("Some memory files couldn’t be loaded. Retry to check for lessons in this selection.")}</p>
          <Button onClick={() => setTick((value) => value + 1)}>{t("Retry lessons")}</Button>
          {filtered && (
            <Button variant="ghost" onClick={reset}>{t("Clear filters")}</Button>
          )}
        </div>
      ) : visible.length === 0 ? (
        <div className="memory-state">
          <h2>{filtered ? t("No matching lessons") : t("No lessons yet")}</h2>
          <p>
            {filtered ? t("Try another search or clear the status and type filters.") : t("Native agents propose lessons. Each lesson’s policy determines the independent reviews or operator approval needed before use.")}
          </p>
          {filtered && <Button onClick={reset}>{t("Show all lessons")}</Button>}
        </div>
      ) : (
        <div className="memory-list">
          {visible.map((memory) => {
            const key = `${memory.project}/${memory.slug}`;
            return (
              <LessonRow
                key={key}
                memory={memory}
                open={open === key}
                onToggle={() => setOpen(open === key ? null : key)}
              />
            );
          })}
        </div>
      )}
    </section>
  );
}
