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
import { initialContext, rememberedContext, rememberContext, subscribeContext } from "../workspace-apps/contextSelection";
import CrmOutlet, { type CrmSection, type OutletView } from "./CrmOutlet";
import { assertRecordId, type HostScope } from "./hostActions";
import "./app-shell.css";

/**
 * The trusted shared App shell (CAD-802): host-owned board surface for
 * every installed App. The board keeps its header/sidebar (App.tsx); the
 * shell adds nested Apps → installed-App navigation, the persistent
 * master conversation on the left with an accessible left drawer at
 * narrow widths, and a generic list/detail/new outlet on the right.
 *
 * One ChatPane instance lives for the shell's lifetime — desktop pane
 * and narrow drawer are one node restyled by CSS, so there is exactly
 * one draft and one SSE subscription, and closing the drawer (which
 * hides it from the tab order via `visibility`) never loses the draft.
 * The shell stays mounted across installation switches, so the chat
 * survives navigation while outlet state resets.
 *
 * Installation and context come from the trusted route plus verified
 * HTTP receipts; record links are context-bound (`ctx` + `record`).
 * The selected record is chat context only — never an authorization
 * claim. Switching installation or context clears the selection, the
 * New view and unsaved drafts. Social-content owns its context
 * selector internally, so the shell shows none there and never a
 * second, divergent one. No App-provided JavaScript runs here.
 */
/** Verified installation identity for the board-level App menu.
 *  Reported only from the shell's HTTP receipt — never the bare
 *  route — so a forged installId cannot conjure menu entries. */
export interface ActiveInstallation {
  installId: string;
  kind: string;
  title: string;
}

export default function AppShell({
  installId,
  viewer,
  children,
  onInstallation,
}: {
  installId: string;
  viewer: Viewer;
  /** Social-content's existing screen; other Apps use the generic outlet. */
  children?: React.ReactNode;
  /** Board menu wiring: receives the verified installation (or null
   *  while loading, failed, or switched away). The parent matches
   *  installId to its route and compares values before storing. */
  onInstallation?: (info: ActiveInstallation | null) => void;
}) {
  const href = useHref();
  const query = useMemo(() => new URLSearchParams(href.split("?")[1] ?? ""), [href]);
  const [installation, setInstallation] = useState<Installation | null>(null);
  const [contexts, setContexts] = useState<AppContext[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [contextId, setContextId] = useState("");
  const [linkNotice, setLinkNotice] = useState<string | null>(null);
  // Social-content's picker owns its selection; the shell observes it
  // in the same tab so chat sends carry the current scope, never the
  // one from the shell's last render.
  const [socialContext, setSocialContext] = useState<string | null>(null);
  // Outlet state lives in the URL (`ctx`, `appview`, `record`) so
  // direct links and browser back keep scope.
  const view: OutletView = query.get("appview") === "new" ? "new" : "list";
  const recordId = query.get("record");
  // CRM nested sections (CAD-784): Apps → CRM → Customers/Segments/
  // Campaigns. Only the CRM outlet reads this — every other App keeps
  // its generic list/detail/new shape untouched.
  const rawSection = query.get("crm");
  const crmSection: CrmSection =
    rawSection === "segments" || rawSection === "campaigns" ? rawSection : "customers";
  const [chatOpen, setChatOpen] = useState(false);
  const chatPaneRef = useRef<HTMLDivElement | null>(null);
  const chatOpenRef = useRef<HTMLButtonElement | null>(null);
  // Installation switches reset outlet state but keep the chat: the
  // first mount preserves direct links, later switches strip them.
  const firstInstall = useRef(installId);
  const handledQuery = useRef<string | undefined>(undefined);

  // Every internal query write marks the resulting key as handled, so
  // the adoption effect below only answers external URL changes
  // (direct links, browser back/forward) — never our own writes.
  const queryKey = (
    ctx: string | null,
    record: string | null,
    appview: string | null,
    crm: string | null,
  ) => `${installId}|${ctx ?? ""}|${record ?? ""}|${appview ?? ""}|${crm ?? ""}`;
  const writeQuery = useCallback(
    (
      patch: {
        ctx?: string | null;
        appview?: OutletView | null;
        record?: string | null;
        crm?: CrmSection | null;
      },
      opts?: { replace?: boolean },
    ) => {
      const [path, search] = href.split("?");
      const q = new URLSearchParams(search ?? "");
      if (patch.ctx !== undefined) {
        if (patch.ctx === null || patch.ctx === "") q.delete("ctx");
        else q.set("ctx", patch.ctx);
      }
      if (patch.appview !== undefined) {
        if (patch.appview === null || patch.appview === "list") q.delete("appview");
        else q.set("appview", patch.appview);
      }
      if (patch.record !== undefined) {
        if (patch.record === null) q.delete("record");
        else q.set("record", patch.record);
      }
      if (patch.crm !== undefined) {
        if (patch.crm === null || patch.crm === "customers") q.delete("crm");
        else q.set("crm", patch.crm);
      }
      handledQuery.current = queryKey(q.get("ctx"), q.get("record"), q.get("appview"), q.get("crm"));
      const s = q.toString();
      navigate(path + (s ? `?${s}` : ""), { replace: opts?.replace });
    },
    [href, installId],
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

  // Board-level App menu identity: report the verified receipt (or
  // null while it is loading, failed, or belongs to another install)
  // so the shared sidebar/phone menu can nest this installation's
  // sections. Clearing on cleanup keeps a departed installation from
  // lingering in host navigation.
  useEffect(() => {
    if (!onInstallation) return;
    if (installation && installation.install_id === installId) {
      onInstallation({
        installId,
        kind: installation.name,
        title: installation.title || installation.name,
      });
    } else {
      onInstallation(null);
    }
    return () => onInstallation(null);
  }, [installId, installation, onInstallation]);

  useEffect(() => {
    setSocialContext(rememberedContext(installId));
    return subscribeContext((changed, next) => {
      if (changed === installId) setSocialContext(next);
    });
  }, [installId]);

  // Installation switch: stale outlet state must never follow the
  // operator across the boundary. The first mount keeps direct links.
  useEffect(() => {
    if (firstInstall.current === installId) return;
    firstInstall.current = installId;
    handledQuery.current = undefined;
    setInstallation(null);
    setContexts([]);
    setLoadError(null);
    setLinkNotice(null);
    setContextId("");
    writeQuery({ ctx: null, appview: null, record: null, crm: null }, { replace: true });
    // The strip marks the emptied query handled: unmark so adoption
    // still runs once the new installation's contexts load.
    handledQuery.current = undefined;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [installId]);

  const activeIds = useMemo(
    () => contexts.filter((c) => c.state === "active").map((c) => c.id),
    [contexts],
  );

  // Adopt the URL's context once contexts load, and on later external
  // URL changes (browser back). A linked context must be active here;
  // a stale, inactive or ambiguous link clears the selection with a
  // notice instead of guessing. Malformed record ids are stripped.
  useEffect(() => {
    // The loaded receipts must belong to this installation: on a
    // switch commit the state still holds the previous install while
    // the URL already names the next one — adopting there would clear
    // or poison the wrong scope.
    if (loading || installation === null || installation.install_id !== installId) return;
    // Social-content owns its context end to end (the shell renders
    // no selector there): adoption must not read, write, or clear its
    // remembered selection.
    if (installation.name === "social-content") return;
    const urlCtx = query.get("ctx");
    const urlRecord = query.get("record");
    const urlView = query.get("appview");
    const urlCrm = query.get("crm");
    if (handledQuery.current === queryKey(urlCtx, urlRecord, urlView, urlCrm)) return;
    handledQuery.current = queryKey(urlCtx, urlRecord, urlView, urlCrm);
    // An unknown CRM section never renders: strip it back to the
    // default instead of guessing a section.
    if (urlCrm !== null && urlCrm !== "segments" && urlCrm !== "campaigns") {
      writeQuery({ crm: null }, { replace: true });
      return;
    }
    if (urlRecord !== null) {
      try {
        assertRecordId(urlRecord);
      } catch {
        writeQuery({ record: null }, { replace: true });
        return;
      }
    }
    if (urlCtx !== null && !activeIds.includes(urlCtx)) {
      setContextId(fallbackContext());
      setLinkNotice(
        "The linked context is not active in this installation — the selection was cleared.",
      );
      writeQuery({ ctx: null, appview: null, record: null }, { replace: true });
      return;
    }
    if (urlCtx !== null) {
      setContextId(urlCtx);
      rememberContext(installId, urlCtx);
      setLinkNotice(null);
      return;
    }
    // No linked context: a record link without scope is ambiguous.
    if (urlRecord !== null) {
      setLinkNotice("The record link names no context — the selection was cleared.");
      setContextId(fallbackContext());
      writeQuery({ record: null }, { replace: true });
      return;
    }
    setContextId(fallbackContext());
  }, [loading, installation, activeIds, query, installId, writeQuery]);

  // The default selection, without persisting an empty choice when
  // this installation has no active contexts to choose from.
  const fallbackContext = () =>
    activeIds.length > 0 ? initialContext(installId, activeIds) : "";

  const pickContext = useCallback(
    (next: string) => {
      if (next === contextId) return;
      setContextId(next);
      rememberContext(installId, next);
      setLinkNotice(null);
      // Context switch clears the selected record and returns the
      // outlet to the list — the outlet remounts on the scope key, so
      // no unsaved draft survives the switch.
      writeQuery({ ctx: next === "" ? null : next, appview: null, record: null });
    },
    [contextId, installId, writeQuery],
  );

  // Narrow drawer focus: opening moves into the pane, closing returns
  // to the trigger. The closed drawer is `visibility: hidden`, so it
  // stays out of the tab order with the draft intact.
  useEffect(() => {
    if (!chatOpen) return;
    chatPaneRef.current?.querySelector("textarea")?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setChatOpen(false);
    };
    addEventListener("keydown", onKey);
    return () => {
      removeEventListener("keydown", onKey);
      chatOpenRef.current?.focus();
    };
  }, [chatOpen]);

  // The App binding for chat sends: install plus the concrete context
  // the shell owns (generic outlet) or the workspace screen owns
  // (social, observed live via subscription). Empty context sends
  // plain chat — there is no App scope to bind. A selected context
  // that is no longer active blocks the send early with a clear
  // message; the server re-proves every binding on send regardless.
  const binding = chatBinding({
    installId,
    wanted: (installation?.name === "social-content" ? socialContext || "" : contextId),
    known: installation !== null && !loading && loadError === null,
    activeIds,
  });
  const scope: HostScope = { installId, contextId };
  // CAD-813: the Campaigns page mints assistant proposal requests
  // against the operator's most recent chat message stamped by the
  // daemon with exactly this scope. The id travels as ordinary
  // shell state — read back from the shared master-thread store,
  // never a global — so a message bound to another install or
  // context can never mint here.
  const scopedChatMessage = useScopedChatMessage(scope);
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
          aria-controls="app-shell-chat"
          onClick={() => setChatOpen((o) => !o)}
        >
          Assistant chat
        </button>
      </div>

      <div className="app-shell-grid">
        <div
          id="app-shell-chat"
          ref={chatPaneRef}
          className="app-shell-chat"
          aria-label="Assistant chat"
          data-open={chatOpen || undefined}
        >
          <div className="app-shell-chat-head">
            <strong className="text-cardtitle text-ink-100">Assistant</strong>
            <button
              type="button"
              className="btn btn-secondary btn-sm app-shell-chat-close"
              onClick={() => setChatOpen(false)}
            >
              Close chat
            </button>
          </div>
          <ChatPane
            viewer={viewer}
            contextLabel={isSocial ? null : contextLabel(contexts, contextId)}
            binding={binding}
          />
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
              {isSocial ? (
                <p className="num text-micro text-ink-500">
                  {installId} · {installation.version} · context is managed inside the workspace screen
                </p>
              ) : (
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
              )}
              {linkNotice && (
                <p className="card px-4 py-3 text-label text-warn border-warn/40" role="alert">
                  {linkNotice}
                </p>
              )}
              {isSocial && children ? (
                children
              ) : (
                <CrmOutlet
                  key={`${installId}:${contextId}`}
                  scope={scope}
                  scopedChatMessage={scopedChatMessage}
                  installationTitle={title}
                  appKind={installation.name === "crm" ? "crm" : "generic"}
                  view={view}
                  recordId={recordId}
                  section={crmSection}
                  viewer={viewer}
                  onView={(v) => writeQuery({ appview: v === "list" ? null : v })}
                  onSelect={(id) => writeQuery({ record: id })}
                  // Section moves ride real submenu links (CrmOutlet):
                  // their hrefs already clear the record view and
                  // drafts, so no callback is needed here.
                  // Created records land on list + details in ONE query
                  // write. Two sequential writes would each start from the
                  // stale render's href, so the second would re-apply the
                  // first's pre-image and retain `appview=new` (CAD-781 F1).
                  onRecordCreated={(id) => writeQuery({ appview: null, record: id })}
                />
              )}
            </>
          )}
        </section>
      </div>

      {chatOpen && (
        <div className="app-shell-scrim" onClick={() => setChatOpen(false)} aria-hidden="true" />
      )}
    </div>
  );
}

export interface ChatScope {
  install_id: string;
  context_id: string;
}

export interface ChatBinding {
  scope: ChatScope | null;
  error: string | null;
}

/** The chat send's App scope: empty wants plain chat, a selected but
 *  inactive context blocks early, otherwise the server proves the
 *  binding on send. Pure — unit-tested through the mounted suite. */
export function chatBinding({ installId, wanted, known, activeIds }: {
  installId: string;
  wanted: string;
  known: boolean;
  activeIds: string[];
}): ChatBinding {
  if (wanted === "") return { scope: null, error: null };
  if (known && !activeIds.includes(wanted)) {
    return {
      scope: null,
      error: `Context “${wanted}” is not active in this installation — pick a current one before sending.`,
    };
  }
  return { scope: { install_id: installId, context_id: wanted }, error: null };
}

function contextLabel(contexts: AppContext[], contextId: string): string | null {
  if (!contextId) return null;
  return contexts.find((c) => c.id === contextId)?.config.label ?? null;
}

/** The daemon-stamped App binding on an entry's payload, if verified. */
export function entryApp(payload: unknown): {
  install_id: string;
  context_id: string;
} | null {
  const app = (payload as { app?: unknown } | null)?.app;
  if (!app || typeof app !== "object") return null;
  const row = app as Record<string, unknown>;
  if (row.verified !== true || typeof row.install_id !== "string" || typeof row.context_id !== "string") {
    return null;
  }
  return { install_id: row.install_id, context_id: row.context_id };
}

/**
 * CAD-813: the most recent operator chat message the daemon stamped
 * with exactly `scope`'s verified App binding — the only
 * `message_id` a proposal-request mint may name. Entries read back
 * from the shared master-thread store are the source: a pending send
 * or a foreign-scope message never qualifies. `null` when the
 * operator has not sent a scoped message in this App yet.
 */
export function latestScopedChatMessage(
  state: { entries?: { role?: string; message?: string | null; payload?: unknown }[] } | null,
  scope: HostScope,
): string | null {
  if (scope.contextId === "") return null;
  const entries = state?.entries ?? [];
  for (let i = entries.length - 1; i >= 0; i--) {
    const entry = entries[i];
    if (entry.role !== "operator" || typeof entry.message !== "string") continue;
    const bound = entryApp(entry.payload);
    if (bound?.install_id === scope.installId && bound.context_id === scope.contextId) {
      return entry.message;
    }
  }
  return null;
}

/** Live view of [`latestScopedChatMessage`] over the shared store. */
function useScopedChatMessage(scope: HostScope): string | null {
  const thread = useQuery(resources.masterThread);
  const [found, setFound] = useState<string | null>(() =>
    latestScopedChatMessage(thread.data, scope),
  );
  const key = `${scope.installId}:${scope.contextId}`;
  useEffect(() => {
    setFound(latestScopedChatMessage(thread.data, scope));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, thread.data]);
  return found;
}

/**
 * The actual master conversation in compact form: the same
 * `resources.masterThread` store Home reads and writes, streamed live
 * from `/api/threads/master/stream`. Whatever the operator says here
 * lands in the same thread Home shows — one conversation, two panes.
 * Sends carry the shell's current installation/context; the daemon
 * proves both against its store and stamps the verified binding on
 * the entry — the chip below renders only that read-back stamp, never
 * what was sent.
 */
function ChatPane({
  viewer,
  contextLabel,
  binding,
}: {
  viewer: Viewer;
  contextLabel: string | null;
  binding: ChatBinding;
}) {
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
    if (binding.error !== null) {
      setSendError(binding.error);
      return;
    }
    const message = newMessageId();
    setSendError(null);
    resources.masterThread.write((s) => addPending(s, message, body, Date.now()));
    api
      .threadSend(MASTER, body, message, undefined, binding.scope ?? undefined)
      .then(() => {
        resources.masterThread.write((s) => settlePending(s, message, { ok: true }));
        void resources.masterState.refresh();
      })
      .catch((e: ApiError) => {
        setSendError(e.message ?? String(e));
        resources.masterThread.write((s) => settlePending(s, message, { ok: false, error: e.message ?? String(e) }));
      });
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
    const bound = item.type === "operator" ? entryApp(item.entry.payload) : null;
    return (
      <p>
        <strong className="text-ink-200">You:</strong> {text}
        {bound && (
          <span className="chip ml-2" title={`Server-verified App context: ${bound.install_id}`}>
            ✓ {bound.context_id}
          </span>
        )}
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
