import { useCallback, useEffect, useRef, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import { useQuery } from "../../lib/useResource";
import { useHref } from "../../lib/useLocation";
import { fmtBytes, fmtTime } from "../../lib/fmt";
import type { OutboxItem } from "../../lib/types";
import Link from "../../ui/Link";
import Md from "../../ui/Md";
import { ResourceGate } from "../../ui/ResourceStatus";

/**
 * The Outbox — what `local`-platform `publish` effects landed once the
 * operator released them (CAD-546). The list is the daemon's
 * `platform_outbox` read, operator-only like every route it relays:
 * an unsigned board gets the route's refusal and this screen says so,
 * it never pretends the ledger is empty.
 *
 * `?item=<effect_id>` deep-links one item (the `board:` link a
 * publish's outcome message carries) — the detail shows the rendered
 * post and the copied attachment names.
 */
export default function Outbox() {
  const state = useQuery(resources.outbox);
  const href = useHref();
  const item = new URLSearchParams(href.split("?")[1] ?? "").get("item");
  return (
    <div className="px-4 lg:px-8 py-5 max-w-3xl">
      <div className="mb-4 flex items-center justify-between gap-3">
        <div>
          <h1 className="text-primary font-medium text-ink-100">Outbox</h1>
          <p className="text-label text-ink-400 mt-0.5">
            posts the operator released to the local outbox
          </p>
        </div>
        <button
          onClick={() => void resources.outbox.refresh()}
          className="chip bg-ink-800 text-ink-300 hover:bg-ink-700 transition-colors"
        >
          refresh
        </button>
      </div>
      <ResourceGate
        state={state}
        loading="loading the outbox…"
        failed="could not load the outbox"
        onRetry={() => void resources.outbox.refresh()}
      />
      {state.status === "failed" && (
        <p className="text-label text-ink-400 -mt-2 mb-4">
          the outbox is the operator's view — sign in with{" "}
          <code className="text-ink-300">cadence ui login</code> to read it
        </p>
      )}
      {state.data &&
        (item ? (
          <OutboxDetailView effectId={item} />
        ) : (
          <OutboxList items={state.data} />
        ))}
    </div>
  );
}

function OutboxList({ items }: { items: OutboxItem[] }) {
  if (items.length === 0) {
    return (
      <div className="card px-4 py-5 text-secondary text-ink-400">
        nothing published yet — a released <code>local/publish</code> effect lands here
      </div>
    );
  }
  return (
    <div className="space-y-2">
      {items.map((it) => (
        <Link
          key={it.effect_id}
          href={`/outbox?item=${encodeURIComponent(it.effect_id)}`}
          className="card block px-4 py-3 hover:border-accent/40 transition-colors"
        >
          <div className="flex items-baseline gap-3 min-w-0">
            <span className="font-medium text-ink-100 truncate">{it.title}</span>
            <span className="num text-micro text-ink-500 shrink-0">
              {it.project} · {fmtTime(it.published_at)}
            </span>
          </div>
          {it.preview && (
            <p className="text-label text-ink-400 mt-1 whitespace-pre-wrap break-words line-clamp-3">
              {it.preview}
            </p>
          )}
          {(it.attachments?.length ?? 0) > 0 && (
            <span className="num text-micro text-ink-500 mt-1 inline-block">
              {it.attachments!.length} attachment{it.attachments!.length === 1 ? "" : "s"}
            </span>
          )}
        </Link>
      ))}
    </div>
  );
}

type Detail =
  | { status: "loading" }
  | { status: "failed"; error: string }
  | { status: "ready"; item: OutboxItem & { post?: string | null } };

function OutboxDetailView({ effectId }: { effectId: string }) {
  const [detail, setDetail] = useState<Detail>({ status: "loading" });
  // A late response for a previously opened item is dropped, never
  // rendered under the wrong id.
  const wanted = useRef(effectId);
  wanted.current = effectId;

  const load = useCallback(() => {
    setDetail({ status: "loading" });
    api
      .outboxItem(effectId)
      .then((d) => {
        if (wanted.current === effectId) setDetail({ status: "ready", item: d.item });
      })
      .catch((e) => {
        if (wanted.current === effectId) {
          setDetail({
            status: "failed",
            error: e instanceof ApiError ? e.message : String(e),
          });
        }
      });
  }, [effectId]);
  useEffect(load, [load]);

  return (
    <div>
      <Link href="/outbox" className="lnk text-label">
        ← all outbox items
      </Link>
      <div className="card mt-2 px-4 py-4">
        {detail.status === "loading" && (
          <span className="text-secondary text-ink-400">loading the item…</span>
        )}
        {detail.status === "failed" && (
          <span className="text-secondary text-fail">could not load the item — {detail.error}</span>
        )}
        {detail.status === "ready" && (
          <div>
            <div className="num text-micro text-ink-500 mb-2">
              {detail.item.project} · {fmtTime(detail.item.published_at)} · {effectId}
            </div>
            {detail.item.post ? (
              <Md text={detail.item.post} />
            ) : (
              <span className="text-secondary text-ink-400">the post did not read back</span>
            )}
            {(detail.item.attachments?.length ?? 0) > 0 && (
              <div className="mt-3 pt-3 border-t border-ink-700">
                <div className="slabel">attachments</div>
                <ul className="mt-1 space-y-0.5">
                  {detail.item.attachments!.map((a) => (
                    <li key={a.name} className="num text-label text-ink-300">
                      {a.name} <span className="text-ink-500">({fmtBytes(a.bytes)})</span>
                    </li>
                  ))}
                </ul>
              </div>
            )}
            {detail.item.path && (
              <div className="num text-micro text-ink-500 mt-3 break-all">{detail.item.path}</div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
