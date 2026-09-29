import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import { streamInto } from "../../lib/sse";
import { useQuery } from "../../lib/useResource";
import { navigate, useHref } from "../../lib/useLocation";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import Select from "../../ui/Select";
import { MASTER } from "../home/master";
import {
  addPending,
  lastSeq,
  newMessageId,
  reduceFrame,
  settlePending,
  stepSummary,
  threadItems,
} from "../home/thread";
import type { Viewer } from "../projects/work";
import { workspaceApps, type AppContext, type Installation } from "../workspace-apps/workspaceApps";
import { initialContext, rememberContext } from "../workspace-apps/contextSelection";
import CrmOutlet, { type OutletView } from "./CrmOutlet";
import type { HostScope } from "./hostActions";
import "./app-shell.css";

/**
 * The trusted shared App shell (CAD-802): host-owned board surface for
 * every installed App. The board keeps its header/sidebar (App.tsx); the
 * shell adds nested Apps → installed-App navigation, the persistent
 * master conversation on the left (the real thread store, not a copy)
 * with an accessible left drawer at narrow widths, and a generic
 * list/detail/new outlet on the right.
 *
 * Installation and context come from the trusted route plus verified
 * HTTP receipts. The selected record is chat context only — never an
 * authorization claim. Switching installation or context clears the
 * selection and unsaved drafts. No App-provided JavaScript runs here;
 * social-content keeps its existing WorkspaceApp as the outlet, other
 * Apps get the generic outlet until their screens land (CAD-781).
 */
export default function AppShell({
  installId,
  viewer,
  children,
}: {
  installId: string;
  viewer: Viewer;
  /** Social-content's existing screen; other Apps use the generic outlet. */
  children?: React.ReactNode;
}) {
  const href = useHref();
  const query = useMemo(() => new URLSearchParams(href.split("?")[1] ?? ""), [href]);
  const [installation, setInstallation] = useState<Installation | null>(null);
  const [contexts, setContexts] = useState<AppContext[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [contextId, setContextId] = useState(() => {
    try {
      return window.sessionStorage.getItem(`cadence.workspace-app.context.${installId}`) ?? "";
    } catch {
      return "";
    }
  });
  // Outlet state: the view and the selected record. Both live in the URL
  // (`appview`, `record`) so direct links and browser back keep scope.
  // Unknown params survive `locationHref`, so these persist across board
  // navigation that carries the query.
  const view: OutletView = query.get("appview") === "new" ? "new" : "list";
  const recordId = query.get("record");
  const [chatOpen, setChatOpen] = useState(false);
  const chatCloseRef = useRef<HTMLButtonElement | null>(null);
  const chatOpenRef = useRef<HTMLButtonElement | null>(null);

  const setQuery = useCallback(
    (patch: { appview?: OutletView | null; record?: string | null }) => {
      const q = new URLSearchParams(href.split("?")[1] ?? "");
      if (patch.appview !== undefined) {
        if (patch.appview === null || patch.appview === "list") q.delete("appview");
        else q.set("appview", patch.appview);
      }
      if (patch.record !== undefined) {
        if (patch.record === null) q.delete("record");
        else q.set("record", patch.record);
      }
      const s = q.toString();
      navigate(href.split("?")[0] + (s ? `?${s}` : ""), { replace: false });
    },
    [href],
  );

  // Verified installation/context receipts. An operator-only read: an
  // unproven viewer sees the sign-in note, never the records.
  useEffect(() => {
    if (!viewer.operator) {
      setLoading(false);
      return;
    }
    const controller = new AbortController();
    setLoading(true);
    setLoadError(null);
    Promise.all([
      workspaceApps.detail(installId, controller.signal),
      workspaceApps.contexts(installId, controller.signal),
    ])
      .then(([next, nextContexts]) => {
        if (controller.signal.aborted) return;
        setInstallation(next);
        setContexts(nextContexts);
        const selected = initialContext(
          installId,
          nextContexts.filter((c) => c.state === "active").map((c) => c.id),
        );
        setContextId(selected);
      })
      .catch((e: unknown) => {
        if (!controller.signal.aborted) {
          setLoadError(e instanceof Error ? e.message : "Could not load this app");
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    return () => controller.abort();
  }, [installId, viewer.operator]);

  // A new installation resets everything: stale selection and drafts
  // must never follow the operator across an install boundary.
  useEffect(() => {
    setInstallation(null);
    setContexts([]);
    setLoadError(null);
  }, [installId]);

  const pickContext = useCallback(
    (next: string) => {
      if (next === contextId) return;
      setContextId(next);
      rememberContext(installId, next);
      // Context switch clears the selected record and returns the
      // outlet to the list — drafts live in the outlet and unmount
      // with it, so nothing unsaved survives the switch.
      setQuery({ appview: null, record: null });
    },
    [contextId, installId, setQuery],
  );

  // Escape closes the narrow chat drawer and returns focus to its trigger.
  useEffect(() => {
    if (!chatOpen) return;
    chatCloseRef.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setChatOpen(false);
    };
    addEventListener("keydown", onKey);
    return () => {
      removeEventListener("keydown", onKey);
      chatOpenRef.current?.focus();
    };
  }, [chatOpen]);

  const scope: HostScope = { installId, contextId };
  const title = installation?.title || installation?.name || "App";
  const isSocial = installation !== null && installation.name === "social-content";

  return (
    <div className="app-shell" data-app-shell-outlet={installId}>
      <div className="app-shell-crumb">
        <Link href="/apps" className="lnk text-label">
          Apps
        </Link>
        <span aria-hidden="true" className="text-ink-600">
          /
        </span>
        <span className="truncate text-ink-100" aria-current="page">
          {loading ? "Loading…" : title}
        </span>
        <span className="flex-1" />
        <button
          ref={chatOpenRef}
          type="button"
          className="btn btn-secondary btn-sm app-shell-chat-toggle"
          aria-expanded={chatOpen}
          aria-controls="app-shell-chat-drawer"
          onClick={() => setChatOpen((o) => !o)}
        >
          Assistant chat
        </button>
      </div>

      <div className="app-shell-grid">
        <div className="app-shell-chat" aria-label="Assistant chat">
          <ChatPane viewer={viewer} contextLabel={contextLabel(contexts, contextId)} />
        </div>
        <section className="app-shell-outlet" aria-label={`${title} workspace`}>
          {loading && (
            <p className="card px-4 py-5 text-secondary text-ink-400" role="status">
              Loading this app…
            </p>
          )}
          {loadError && (
            <p className="card px-4 py-5 text-secondary text-fail border-fail/40" role="alert">
              {loadError}{" "}
              <Button size="sm" onClick={() => location.reload()}>
                Retry
              </Button>
            </p>
          )}
          {!loading && !installation && !loadError && !viewer.operator && (
            <main className="card px-4 py-5" aria-label="App">
              <h2 className="text-cardtitle font-medium text-ink-100">
                {title}
              </h2>
              <p className="text-secondary text-ink-400 mt-1">
                Sign in as the operator to inspect this installation.
              </p>
              <p className="mt-2">
                <Link href="/apps" className="lnk text-label">
                  ← All apps
                </Link>
              </p>
            </main>
          )}
          {installation && (
            <>
              <div className="app-shell-context">
                <Select
                  value={contextId}
                  onChange={pickContext}
                  options={[
                    { value: "", label: "No context" },
                    ...contexts
                      .filter((c) => c.state === "active")
                      .map((c) => ({ value: c.id, label: c.config.label })),
                  ]}
                  aria-label="App context"
                  disabled={!viewer.operator || viewer.readOnly}
                  full
                />
                <p className="num text-micro text-ink-500" title="Verified installation digest">
                  {installId} · {installation.version}
                </p>
              </div>
              {isSocial && children ? (
                children
              ) : (
                <CrmOutlet
                  scope={scope}
                  installationTitle={title}
                  view={view}
                  recordId={recordId}
                  viewer={viewer}
                  onView={(v) => setQuery({ appview: v === "list" ? null : v })}
                  onSelect={(id) => setQuery({ record: id })}
                />
              )}
            </>
          )}
        </section>
      </div>

      {chatOpen && (
        <div className="app-shell-scrim" onClick={() => setChatOpen(false)} aria-hidden="true" />
      )}
      <div
        id="app-shell-chat-drawer"
        className="app-shell-drawer"
        role="dialog"
        aria-modal="true"
        aria-label="Assistant chat"
        data-open={chatOpen || undefined}
      >
        <div className="app-shell-drawer-head">
          <strong className="text-cardtitle text-ink-100">Assistant</strong>
          <button
            ref={chatCloseRef}
            type="button"
            className="btn btn-secondary btn-sm"
            onClick={() => setChatOpen(false)}
          >
            Close chat
          </button>
        </div>
        <ChatPane viewer={viewer} contextLabel={contextLabel(contexts, contextId)} />
      </div>
    </div>
  );
}

function contextLabel(contexts: AppContext[], contextId: string): string | null {
  if (!contextId) return null;
  return contexts.find((c) => c.id === contextId)?.config.label ?? null;
}

/**
 * The actual master conversation in compact form: the same
 * `resources.masterThread` store Home reads and writes, streamed live
 * from `/api/threads/master/stream`. Whatever the operator says here
 * lands in the same thread Home shows — one conversation, two panes.
 */
function ChatPane({ viewer, contextLabel }: { viewer: Viewer; contextLabel: string | null }) {
  const thread = useQuery(resources.masterThread);
  const [draft, setDraft] = useState("");
  const [sendError, setSendError] = useState<string | null>(null);
  const loaded = thread.data !== null;

  useEffect(() => {
    if (!loaded) void resources.masterThread.refresh();
  }, [loaded]);
  useEffect(() => {
    if (!loaded || thread.data?.missing === true) return;
    const sub = streamInto(resources.masterThread, reduceFrame, {
      url: `/api/threads/${MASTER}/stream`,
      events: ["entry"],
      lastEventId: String(lastSeq(resources.masterThread.get().data) ?? 0),
      onError: () => undefined,
    });
    return () => sub.close();
  }, [loaded, thread.data?.missing]);

  const items = threadItems(thread.data);
  const tail = items.slice(-8);
  const canSend = viewer.operator && !viewer.readOnly;

  const send = () => {
    const body = draft.trim();
    if (!body || !canSend) return;
    const message = newMessageId();
    setSendError(null);
    resources.masterThread.write((s) => addPending(s, message, body, Date.now()));
    api
      .threadSend(MASTER, body, message)
      .then(() => {
        resources.masterThread.write((s) => settlePending(s, message, { ok: true }));
        void resources.masterState.refresh();
      })
      .catch((e: ApiError) =>
        resources.masterThread.write((s) => settlePending(s, message, { ok: false, error: e.message ?? String(e) })),
      );
    setDraft("");
  };

  return (
    <div className="app-chat" data-chat-pane>
      <p className="text-micro text-ink-500">
        Master thread{contextLabel ? ` · ${contextLabel}` : ""} — context for this turn, never access proof.
      </p>
      {thread.status === "failed" && (
        <p className="text-label text-fail" role="alert">
          The thread could not be read — {thread.error}{" "}
          <button type="button" className="lnk" onClick={() => void resources.masterThread.refresh()}>
            Retry
          </button>
        </p>
      )}
      {!loaded && thread.status !== "failed" && (
        <p className="text-label text-ink-500" role="status">
          Reading the thread…
        </p>
      )}
      {loaded && tail.length === 0 && (
        <p className="text-label text-ink-500" data-empty="chat">
          No conversation yet. Ask the master from Home, or send the first message here.
        </p>
      )}
      <ol className="app-chat-list" aria-label="Recent master messages">
        {tail.map((item) => (
          <li key={item.key} className="text-secondary text-ink-300 break-words">
            <ChatRow item={item} />
          </li>
        ))}
      </ol>
      {sendError && (
        <p className="text-label text-fail" role="alert">
          {sendError}
        </p>
      )}
      <form
        className="app-chat-form"
        onSubmit={(e) => {
          e.preventDefault();
          send();
        }}
      >
        <label className="sr-only" htmlFor="app-shell-chat-box">
          Message to the master
        </label>
        <textarea
          id="app-shell-chat-box"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              send();
            }
          }}
          rows={2}
          disabled={!canSend}
          placeholder={canSend ? "Ask Master… (Enter sends)" : "Read-only · Sending is unavailable"}
          aria-label="Message to the master"
          className="app-chat-box"
        />
        <Button type="submit" variant="primary" size="sm" disabled={!canSend || !draft.trim()}>
          Send
        </Button>
      </form>
    </div>
  );
}

function ChatRow({ item }: { item: ReturnType<typeof threadItems>[number] }) {
  if (item.type === "operator" || item.type === "pending") {
    const text = item.type === "operator" ? item.entry.text : item.pending.text;
    return (
      <p>
        <strong className="text-ink-200">You:</strong> {text}
      </p>
    );
  }
  if (item.type === "answer") {
    return (
      <p>
        <strong className="text-ink-200">Master:</strong> {(stepSummary(item.entry.text) || item.entry.text).slice(0, 280)}
      </p>
    );
  }
  if (item.type === "commentary") {
    return <p className="italic text-ink-400">{item.entry.text.slice(0, 280)}</p>;
  }
  if (item.type === "system") {
    return <p className="text-micro text-ink-500">· {stepSummary(item.entry.text).slice(0, 160)}</p>;
  }
  return <p className="text-micro text-ink-500">· {item.entries.length} tool steps</p>;
}
