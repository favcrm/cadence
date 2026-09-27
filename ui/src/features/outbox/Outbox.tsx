import { useEffect, useId, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import { useMaybeResource } from "../../lib/useResource";
import { navigate, useHref } from "../../lib/useLocation";
import { fmtBytes } from "../../lib/fmt";
import type { OutboxItem } from "../../lib/types";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import Md from "../../ui/Md";
import Select from "../../ui/Select";
import { IconCaret, IconRefresh, IconSearch } from "../../ui/icons";
import {
  outboxHref,
  postTitle,
  publicationTime,
  readOutboxFilters,
  visiblePosts,
} from "./outboxView";
import "./outbox.css";

/** Local publication history. Protected reads require proven operator access. */
export default function Outbox({
  operator,
  onOpenIssue,
}: {
  operator: boolean;
  onOpenIssue?: (id: string) => void;
}) {
  const href = useHref();
  const queryStart = href.indexOf("?");
  const search = queryStart >= 0 ? href.slice(queryStart + 1) : "";
  const filters = readOutboxFilters(search);
  // A direct item link does not depend on the collection being available.
  const operatorOutbox = operator ? resources.outbox : null;
  const resource = filters.item ? null : operatorOutbox;
  const state = useMaybeResource(resource);
  const searchId = useId();
  const projectId = useId();
  useEffect(() => {
    if (resource) void resource.revalidate();
  }, [resource]);

  const items = state?.data;
  const visible = items
    ? visiblePosts(items, filters.query, filters.project)
    : [];
  const projects = [...new Set(items?.map((item) => item.project) ?? [])];
  if (filters.project && !projects.includes(filters.project))
    projects.push(filters.project);
  projects.sort();
  const filtered = Boolean(filters.query || filters.project);
  const clearFilters = () =>
    navigate(outboxHref(search, { query: "", project: "" }), { replace: true });

  return (
    <section
      className="outbox-page px-4 lg:px-8 py-5"
      aria-labelledby="outbox-title"
    >
      <div className="outbox-heading">
        <div>
          <h1
            id="outbox-title"
            className="text-section font-medium text-ink-100"
          >
            Outbox
          </h1>
          <p className="text-label text-ink-400 mt-1">
            Released posts saved to your local outbox.
          </p>
        </div>
        {resource && (
          <Button
            variant="ghost"
            className="outbox-refresh"
            icon={<IconRefresh />}
            aria-label="Refresh outbox"
            title="Refresh outbox"
            loading={state?.inFlight}
            onClick={() => void resource.refresh()}
          />
        )}
      </div>

      {!operator ? (
        <div className="card outbox-notice">
          <div>
            <h2 className="text-secondary font-medium text-ink-100 mb-1">
              Sign in to view published posts
            </h2>
            <p>
              Outbox is available to the operator. Use Sign in in the top bar to
              connect with a login link.
            </p>
          </div>
        </div>
      ) : filters.item ? (
        <OutboxDetailView
          key={filters.item}
          effectId={filters.item}
          backHref={outboxHref(search, { item: null })}
          onOpenIssue={onOpenIssue}
        />
      ) : (
        <>
          {items && (items.length > 0 || filtered) && (
            <div className="outbox-tools">
              <div className="outbox-search">
                <label htmlFor={searchId}>Search posts</label>
                <div className="outbox-search-box">
                  <IconSearch />
                  <input
                    id={searchId}
                    type="search"
                    className="field"
                    value={filters.query}
                    placeholder="Title, preview or attachment"
                    autoComplete="off"
                    onChange={(event) =>
                      navigate(
                        outboxHref(search, { query: event.target.value }),
                        { replace: true },
                      )
                    }
                  />
                </div>
              </div>
              {(projects.length > 1 || filters.project) && (
                <div className="outbox-project">
                  <label className="outbox-project-label" htmlFor={projectId}>
                    Project
                  </label>
                  <Select
                    id={projectId}
                    aria-label="Project"
                    value={filters.project}
                    full
                    options={[
                      { value: "", label: "All projects" },
                      ...projects.map((key) => ({ value: key, label: key })),
                    ]}
                    onChange={(project) =>
                      navigate(outboxHref(search, { project }), {
                        replace: true,
                      })
                    }
                  />
                </div>
              )}
              {filtered && (
                <Button variant="ghost" onClick={clearFilters}>
                  Clear filters
                </Button>
              )}
            </div>
          )}
          {state?.status === "stale" && (
            <div className="card outbox-notice outbox-warning" role="status">
              <div>
                Refresh failed. Showing the last loaded posts
                {state.asOf
                  ? ` from ${publicationTime(new Date(state.asOf).toISOString())}`
                  : ""}
                .
              </div>
              <Button
                onClick={() => resource && void resource.refresh()}
                loading={state.inFlight}
              >
                Retry
              </Button>
            </div>
          )}
          {state && !items && (
            <div
              className="card outbox-notice"
              role={state.status === "failed" ? "alert" : "status"}
            >
              <div>
                {state.status === "failed" ? (
                  <>
                    <p className="text-ink-100 font-medium">
                      Could not load published posts
                    </p>
                    <p className="mt-1">
                      {state.error ?? "Try again in a moment."}
                    </p>
                  </>
                ) : (
                  "Loading published posts…"
                )}
              </div>
              {state.status === "failed" && (
                <Button
                  onClick={() => resource && void resource.refresh()}
                  loading={state.inFlight}
                >
                  Retry
                </Button>
              )}
            </div>
          )}
          {items && (
            <>
              {items.length > 0 && (
                <div className="outbox-summary" role="status">
                  <span>
                    {filtered
                      ? `${visible.length} of ${items.length}`
                      : items.length}{" "}
                    post{items.length === 1 ? "" : "s"}
                  </span>
                  <span>Newest first</span>
                </div>
              )}
              {visible.length ? (
                <ul className="outbox-list" aria-label="Published posts">
                  {visible.map((item) => (
                    <li key={item.effect_id}>
                      <Link
                        href={outboxHref(search, { item: item.effect_id })}
                        className="card outbox-post-link"
                      >
                        <span className="outbox-post-title">
                          {postTitle(item)}
                        </span>
                        <span className="outbox-post-meta">
                          <span>{item.project}</span>
                          <time dateTime={item.published_at}>
                            {publicationTime(item.published_at)}
                          </time>
                        </span>
                        {item.preview && (
                          <p className="outbox-post-preview">{item.preview}</p>
                        )}
                        {!!item.attachments?.length && (
                          <span className="outbox-attachment-count">
                            {item.attachments.length} attachment
                            {item.attachments.length === 1 ? "" : "s"}
                          </span>
                        )}
                      </Link>
                    </li>
                  ))}
                </ul>
              ) : (
                <div className="card outbox-empty">
                  <h2 className="text-secondary font-medium text-ink-100 mb-1">
                    {items.length
                      ? "No matching posts"
                      : "No published posts yet"}
                  </h2>
                  <p className="text-label text-ink-400">
                    {items.length
                      ? "Try another search or project, or clear your filters."
                      : "Posts appear here after you release a publication to the local outbox."}
                  </p>
                </div>
              )}
            </>
          )}
        </>
      )}
    </section>
  );
}

type Detail =
  | { status: "loading" }
  | { status: "failed"; error: string }
  | { status: "ready"; item: OutboxItem & { post?: string | null } };

function OutboxDetailView({
  effectId,
  backHref,
  onOpenIssue,
}: {
  effectId: string;
  backHref: string;
  onOpenIssue?: (id: string) => void;
}) {
  const [detail, setDetail] = useState<Detail>({ status: "loading" });
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let live = true;
    setDetail({ status: "loading" });
    api
      .outboxItem(effectId)
      .then((result) => {
        if (live) setDetail({ status: "ready", item: result.item });
      })
      .catch((error) => {
        if (live)
          setDetail({
            status: "failed",
            error: error instanceof ApiError ? error.message : String(error),
          });
      });
    // Includes navigation A → B → A, retry, unmount and revoked operator access.
    return () => {
      live = false;
    };
  }, [effectId, attempt]);
  const retry = () => setAttempt((value) => value + 1);

  return (
    <div className="outbox-detail">
      <Button
        variant="ghost"
        href={backHref}
        icon={<IconCaret size={14} style={{ transform: "rotate(180deg)" }} />}
      >
        Back to posts
      </Button>
      {detail.status !== "ready" ? (
        <div
          className="card outbox-notice mt-4"
          role={detail.status === "failed" ? "alert" : "status"}
        >
          <div>
            {detail.status === "failed" ? (
              <>
                <p className="font-medium text-ink-100">
                  Could not load this post
                </p>
                <p className="mt-1">{detail.error}</p>
              </>
            ) : (
              "Loading post…"
            )}
          </div>
          {detail.status === "failed" && <Button onClick={retry}>Retry</Button>}
        </div>
      ) : (
        <article className="card outbox-detail-card">
          <header className="outbox-detail-heading">
            <div className="flex justify-between items-center gap-3">
              <span className="text-micro font-medium text-ink-400">
                Published locally
              </span>
              <Button
                variant="ghost"
                icon={<IconRefresh />}
                aria-label="Refresh post"
                title="Refresh post"
                onClick={retry}
              />
            </div>
            <h2 className="text-drawer font-medium text-ink-100">
              {postTitle(detail.item)}
            </h2>
            <div className="outbox-detail-meta">
              <span>{detail.item.project}</span>
              <time dateTime={detail.item.published_at}>
                {publicationTime(detail.item.published_at)}
              </time>
            </div>
          </header>
          <div className="outbox-post-content issue-reader text-body text-ink-200">
            {typeof detail.item.post === "string" ? (
              detail.item.post ? (
                <Md text={detail.item.post} onOpen={onOpenIssue} />
              ) : (
                <p className="text-label text-ink-400">
                  This post has no text content.
                </p>
              )
            ) : (
              <div className="outbox-notice" role="status">
                <div>
                  The saved post content could not be read. Its publication
                  details are still available.
                </div>
                <Button onClick={retry}>Retry content</Button>
              </div>
            )}
          </div>
          {!!detail.item.attachments?.length && (
            <section className="outbox-attachments" aria-label="Attachments">
              <h3 className="text-secondary font-medium text-ink-100">
                Attachments{" "}
                <span className="text-ink-500">
                  ({detail.item.attachments.length})
                </span>
              </h3>
              <ul>
                {detail.item.attachments.map((attachment) => (
                  <li key={attachment.name}>
                    <span>{attachment.name}</span>
                    <span className="text-ink-500">
                      {fmtBytes(attachment.bytes)}
                    </span>
                  </li>
                ))}
              </ul>
            </section>
          )}
          <details className="outbox-storage">
            <summary>Publication details</summary>
            <dl>
              <dt>Publication ID</dt>
              <dd className="num">{effectId}</dd>
              {detail.item.path && (
                <>
                  <dt>Local folder</dt>
                  <dd className="num">{detail.item.path}</dd>
                </>
              )}
            </dl>
          </details>
        </article>
      )}
    </div>
  );
}
