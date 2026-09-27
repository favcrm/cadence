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

const STATUSES = ["proposed", "accepted", "rejected", "superseded"];
const TYPES = ["rule", "gotcha", "decision", "recipe"];
const label = (value: string) => value.charAt(0).toUpperCase() + value.slice(1);
const options = (values: string[], all: string) => [
  { value: "", label: all },
  ...values.map((value) => ({ value, label: label(value) })),
];
const errText = (e: unknown) => (e instanceof ApiError ? e.message : String(e));

function scopeLine(m: MemoryCard): string {
  const bits: string[] = [];
  if (m.scope.project) bits.push("Project-wide");
  if (m.scope.components.length)
    bits.push(`Components: ${m.scope.components.join(", ")}`);
  if (m.scope.paths.length) bits.push(`Paths: ${m.scope.paths.join(", ")}`);
  if (m.scope.providers.length)
    bits.push(`Providers: ${m.scope.providers.join(", ")}`);
  if (m.scope.tags.length) bits.push(`Tags: ${m.scope.tags.join(", ")}`);
  return bits.join(" · ") || "No scope recorded";
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
  ]
    .join(" ")
    .toLowerCase();
  return text.includes(query.trim().toLowerCase());
}

function createdLabel(created: string): string {
  const date = new Date(created);
  return Number.isNaN(date.getTime())
    ? "Not recorded"
    : date.toLocaleDateString(undefined, {
        year: "numeric",
        month: "short",
        day: "numeric",
        timeZone: "UTC",
      });
}

/** Each open selection owns its request lifetime, including reopening the same key. */
function LessonDetail({ memory }: { memory: MemoryCard }) {
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
          Couldn’t load this lesson. <span>{error}</span>
        </p>
        <Button onClick={() => setRetry((value) => value + 1)}>
          Retry lesson
        </Button>
      </div>
    );
  if (!detail)
    return (
      <p className="memory-detail-state" role="status">
        Loading lesson…
      </p>
    );
  const q = quorumView(detail);
  return (
    <div className="memory-detail-content">
      <div className="memory-reading">
        <div className="issue-reader">
          <Md text={detail.body} />
        </div>
        <div className="memory-applicability">
          <h3>Applies to</h3>
          <p>{scopeLine(detail)}</p>
          {detail.supersedes && <p>Supersedes {detail.supersedes}</p>}
        </div>
      </div>
      <aside
        className="memory-evidence"
        aria-label="Lesson review and evidence"
      >
        <h3>
          {q.mode === "acceptance" ? "Review status" : "Agent availability"}
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
        <p className="memory-readonly">
          Read-only here. Authenticated native agents review lessons; a PM
          finalizes them.
        </p>
        <details className="memory-provenance">
          <summary>
            Review evidence <IconChevron />
          </summary>
          <dl>
            <div>
              <dt>Lesson ID</dt>
              <dd className="num">{detail.slug}</dd>
            </div>
            <div>
              <dt>Created</dt>
              <dd>{createdLabel(detail.created)}</dd>
            </div>
            <div>
              <dt>Confidence</dt>
              <dd>{label(detail.confidence)}</dd>
            </div>
            <div>
              <dt>Author</dt>
              <dd>{detail.author || "Not recorded"}</dd>
            </div>
            <div>
              <dt>Source</dt>
              <dd>{detail.source || "Not recorded"}</dd>
            </div>
            <div>
              <dt>File</dt>
              <dd className="num">{detail.path}</dd>
            </div>
            <div>
              <dt>Revision digest</dt>
              <dd className="num">{detail.revision_digest || "Unavailable"}</dd>
            </div>
            {typeof detail.review_count === "number" && (
              <div>
                <dt>Historical receipts</dt>
                <dd>{detail.review_count}</dd>
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
          <p className="memory-scope">{scopeLine(memory)}</p>
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
    { value: "all", label: "All projects" },
    ...projects.map((p) => ({ value: p.key, label: p.key })),
    ...(project !== "all" && !projects.some((p) => p.key === project)
      ? [{ value: project, label: project }]
      : []),
  ];

  return (
    <section className="memory-workspace" aria-labelledby="memory-title">
      <header className="memory-heading">
        <div>
          <h1 id="memory-title">Memory</h1>
          <p>
            Project lessons that help agents apply what the team has learned.
          </p>
        </div>
        <Button
          icon={<IconRefresh />}
          loading={mems === null}
          onClick={() => setTick((value) => value + 1)}
        >
          Refresh lessons
        </Button>
      </header>
      <div className="memory-filters" role="search" aria-label="Find lessons">
        <div className="memory-filter memory-search">
          <label htmlFor={searchId}>Search lessons</label>
          <div className="memory-search-field">
            <IconSearch />
            <input
              id={searchId}
              type="search"
              className="field"
              placeholder="Search claims, IDs or scope…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
          </div>
        </div>
        {showProjectChoices(projects.length, project) && (
          <div className="memory-filter">
            <label htmlFor={projectId}>Project</label>
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
          <label htmlFor={statusId}>Status</label>
          <Select
            id={statusId}
            full
            value={status}
            options={options(STATUSES, "All statuses")}
            onChange={setStatus}
          />
        </div>
        <div className="memory-filter">
          <label htmlFor={typeId}>Type</label>
          <Select
            id={typeId}
            full
            value={kind}
            options={options(TYPES, "All types")}
            onChange={setKind}
          />
        </div>
      </div>
      <div className="memory-results">
        <p role="status">
          {mems === null
            ? "Loading lessons…"
            : listErr
              ? "Lessons unavailable"
              : `${visible.length} lesson${visible.length === 1 ? "" : "s"}${query.trim() ? ` matching “${query.trim()}”` : ""}`}
        </p>
        {filtered && (
          <Button variant="ghost" onClick={reset}>
            Clear filters
          </Button>
        )}
      </div>
      {loadErrs.length > 0 && (
        <div className="memory-load-warning" role="status">
          <p>
            {loadErrs.length} memory file{loadErrs.length === 1 ? "" : "s"}{" "}
            couldn’t be loaded. Results may be incomplete.
          </p>
          <details>
            <summary>Show file errors</summary>
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
          <h2>Couldn’t load lessons</h2>
          <p>{listErr}</p>
          <Button onClick={() => setTick((value) => value + 1)}>
            Retry lessons
          </Button>
        </div>
      ) : mems === null ? (
        <div className="memory-state" aria-busy="true">
          <p>Loading project memory…</p>
        </div>
      ) : visible.length === 0 ? (
        <div className="memory-state">
          <h2>{filtered ? "No matching lessons" : "No lessons yet"}</h2>
          <p>
            {filtered
              ? "Try another search or clear the status and type filters."
              : "Native agents propose lessons, independent agents review them, and a PM finalizes them before use."}
          </p>
          {filtered && <Button onClick={reset}>Show all lessons</Button>}
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
