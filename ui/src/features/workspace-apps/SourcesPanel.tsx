import { useEffect, useState } from "react";
import Button from "../../ui/Button";
import { ApiError } from "../../lib/api";
import { workspaceApps, type SourcePost, type SourceReceipt, type WorkspaceRun } from "./workspaceApps";
import type { SelectedSource } from "./NewPost";

function verifiedReceipt(receipt: SourceReceipt, run: WorkspaceRun): boolean {
  const result = receipt.result;
  return run.approved_digest === run.snapshot_digest
    && receipt.run_id === run.id && receipt.slot === "source"
    && typeof receipt.digest === "string" && receipt.digest.length > 0
    && receipt.binding_digest === run.snapshot.capabilities?.source?.digest
    && result?.schema === 1 && result.kind === "social.source.posts"
    && result.provider === "agenticos_external"
    && result.source_tool === "scrapecreators.instagram.user.posts"
    && result.handle === run.snapshot.inputs.profile_handle && Array.isArray(result.posts)
    && result.posts.length <= 24
    && (result.profile_verified === true || (result.profile_verified === false && result.posts.length === 0))
    && result.posts.every(post => typeof post.id === "string" && post.id.length > 0
      && typeof post.caption === "string" && typeof post.permalink === "string"
      && /^https:\/\/www\.instagram\.com\/p\/[A-Za-z0-9_-]+\/$/.test(post.permalink));
}

function safePreviewUrl(value: string | null | undefined): string | null {
  if (!value) return null;
  try {
    const url = new URL(value);
    const host = url.hostname.toLowerCase();
    return url.protocol === "https:" && !url.username && !url.password
      && (host === "cdninstagram.com" || host.endsWith(".cdninstagram.com")
        || host === "fbcdn.net" || host.endsWith(".fbcdn.net")) ? value : null;
  } catch {
    return null;
  }
}

function SourcePostPreview({ post }: { post: SourcePost }) {
  const [failed, setFailed] = useState(false);
  const url = safePreviewUrl(post.preview_url);
  if (url && !failed)
    return <img className="wa-source-image" src={url} alt="" loading="lazy" referrerPolicy="no-referrer" onError={() => setFailed(true)} />;
  return <div className="wa-source-image-fallback" aria-label="Source image preview unavailable">
    <span aria-hidden="true">▧</span><span>Preview unavailable · inspect the original post</span>
  </div>;
}

export function SourcesPanel({ runs, canWrite, canCreate, imageSupported, onImport, onOpenRun, onPick, onDenied }: {
  runs: WorkspaceRun[];
  canWrite: boolean;
  canCreate: boolean;
  imageSupported?: boolean;
  onImport: () => void;
  onOpenRun: (run: WorkspaceRun) => void;
  onPick: (source: SelectedSource) => void;
  onDenied: () => void;
}) {
  const [receipts, setReceipts] = useState<Record<string, SourceReceipt[]>>({});
  const [errors, setErrors] = useState<Record<string, string>>({});
  useEffect(() => {
    const controller = new AbortController();
    setReceipts({}); setErrors({});
    for (const run of runs.filter(value => value.state === "succeeded" || value.state === "failed")) {
      void workspaceApps.capabilityResults(run.id, controller.signal).then(list => {
        if (!controller.signal.aborted)
          setReceipts(previous => ({ ...previous, [run.id]: list.filter(value => verifiedReceipt(value, run)) }));
      }).catch(error => {
        if (controller.signal.aborted) return;
        if (error instanceof ApiError && [401, 403].includes(error.status)) {
          onDenied(); return;
        }
        setErrors(previous => ({ ...previous, [run.id]: error instanceof Error ? error.message : "Could not read source receipts." }));
      });
    }
    return () => controller.abort();
  }, [runs.map(value => `${value.id}:${value.state}`).join("|"), onDenied]);
  return <section className="wa-stack wa-sources" aria-label="Instagram sources">
    <div className="wa-row">
      <div>
        <h2>Public Instagram sources</h2>
        <p className="wa-muted">One explicit profile read creates a retained receipt. Choose one verified post to start a {imageSupported ? "caption or priced caption-and-image" : "caption"} plan.</p>
      </div>
      <Button onClick={onImport} disabled={!canWrite}>Find public posts</Button>
    </div>
    {!runs.length && <p className="wa-panel wa-empty">No source reads yet. Bind an AgenticOS provider connection in Settings, then find a public profile.</p>}
    {runs.map(run => {
      const list = receipts[run.id];
      return <section key={run.id} className="wa-panel wa-stack">
        <div className="wa-row">
          <strong>@{run.snapshot.inputs.profile_handle}</strong>
          <span className="wa-status">{run.state.replaceAll("_", " ")}</span>
          <Button size="sm" onClick={() => onOpenRun(run)}>Inspect run</Button>
        </div>
        {errors[run.id] && <p className="wa-alert" data-tone="fail" role="alert">{errors[run.id]}</p>}
        {run.state === "failed" && list && list.length > 0 && <p className="wa-alert" data-tone="fail">The source run failed after the retained read. Verified posts remain available to start a new caption plan; inspect the run for the later failure.</p>}
        {run.state === "failed" && list?.length === 0 && <p className="wa-alert" data-tone="fail">The source run failed without a usable retained read. Inspect the run before starting another paid read.</p>}
        {(run.state === "succeeded" || run.state === "failed") && !list && !errors[run.id] && <p className="wa-muted" role="status">Loading retained source receipts…</p>}
        {list?.map(receipt => <div key={receipt.id} className="wa-stack">
          <p className="wa-kicker">Receipt {receipt.id} · @{receipt.result.handle} · {receipt.result.more_available ? "more posts available at provider" : "page complete"}</p>
          {receipt.result.posts.length === 0 && <p className="wa-alert" role="status">{receipt.result.profile_verified
            ? "No public posts were returned for this verified profile. Try another public profile or add source facts manually."
            : "No public posts were returned. The profile identity cannot be verified from an empty page. Try another public profile or add source facts manually."}</p>}
          {receipt.result.posts.length > 0 && <div className="wa-source-grid">
            {receipt.result.posts.map(post => <article key={post.id} className="wa-post-card">
              <SourcePostPreview post={post} />
              <div className="wa-card-meta"><span className="wa-kicker">{post.media_kind}</span><time dateTime={post.published_at ?? undefined}>{post.published_at ? new Date(post.published_at).toLocaleDateString() : post.published_at_unix ? new Date(post.published_at_unix * 1000).toLocaleDateString() : "Time unavailable"}</time></div>
              <p className="wa-source-caption">{post.caption || "This post has no caption."}</p>
              <div className="wa-row">
                <a className="wa-source-original" href={post.permalink} target="_blank" rel="noopener noreferrer">View original post ↗</a>
                <Button size="sm" disabled={!canCreate || !post.caption} onClick={() => onPick({ receiptId: receipt.id, postId: post.id, handle: receipt.result.handle, caption: post.caption, permalink: post.permalink })}>Use as source</Button>
              </div>
            </article>)}
          </div>}
        </div>)}
        {list?.length === 0 && <p className="wa-muted">No usable source receipt was stored for this run.</p>}
      </section>;
    })}
  </section>;
}
