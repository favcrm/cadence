import { chatContext } from "../chatScreen";
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { api, ApiError } from "../../../lib/api";
import { resources } from "../../../lib/resources";
import { streamInto } from "../../../lib/sse";
import { useQuery, useResource } from "../../../lib/useResource";
import Button from "../../../ui/Button";
import type { ThreadRef } from "../../../lib/types";
import { MASTER } from "../../home/master";
import { ThreadItemView, type Density } from "../../home/ThreadView";
import {
  addPending,
  discardPending,
  lastSeq,
  newMessageId,
  reduceFrame,
  settlePending,
  stepSummary,
  threadItems,
  toolSteps,
  type ThreadItem,
} from "../../home/thread";
import type { Viewer } from "../../projects/work";
import {
  QUEUED_NOTICE,
  conversationLabel,
  conversationStreamUrl,
  conversationThread,
  createConversation,
  hasUnansweredOperator,
  idleThread,
  isQueuedBehindOther,
  parseSlash,
  selectConversation,
  useActiveConversation,
} from "../conversationClient";
import type { ActionContext } from "./actions";
import { renderCapability } from "./capabilities";
import type { AppChat } from "./contract";
import ChatFrame from "./ChatFrame";
import DirectiveCard, { ConfirmationCard } from "./DirectiveCard";
import AssistantOperations from "./AssistantOperations";
import { hideIds, matchDirective, type Directive } from "./directive";
import { planFrames, type FrameRow } from "./frames";
import type { ChatBinding } from "./types";

/**
 * The host's ONE Conversation (CAD-1108): Home and every installed app render
 * their assistant chat through it, so a fix lands in both. It is built on the
 * thread renderer (`ThreadItemView`) and adds only what an app pane needs: the
 * CAD-1098 conversation picker, the composer, the app-mode guards (internal
 * ids hidden, tool steps folded, token-bearing bodies never printed raw) and
 * the data an app describes with its validated `app-chat/v1` descriptor
 * (context chip and prompts, host capability attachments, declared cards and
 * screens). Nothing here names an app: with no descriptor the pane is plain
 * shared chat, and a descriptor can only choose among host behaviours.
 *
 * `home` renders the list only, byte-for-byte what Home rendered before; Home
 * keeps its own scroller and composer.
 */
export { chatContext };
export type ConversationMode =
  | { kind: "home" }
  | {
      kind: "app";
      /** All four come from the route and the shell's own state, never from
       *  a message, a descriptor or a stored draft. */
      installId: string;
      contextId: string;
      screen: string | null;
      recordOpen: boolean;
      /** The route context's display name, shown only when the descriptor asks. */
      contextName: string | null;
      /** The validated descriptor for this install, or null (plain chat). */
      descriptor: AppChat | null;
      /** `list` lays the screen's static prompts out as suggestion rows and
       *  drops the context chip; default is the compact chip row. */
      promptLayout?: "chips" | "list";
    };
type AppMode = Extract<ConversationMode, { kind: "app" }>;

export interface HomeListProps {
  mode: { kind: "home" };
  items: ThreadItem[];
  readOnly: boolean;
  liveAfter: number;
  onOpenIssue: (id: string) => void;
  onRetry: (message: string, text: string, refs?: ThreadRef[]) => void;
  onDiscard: (message: string) => void;
  /** Extra content under a row (Home's plan cards). */
  after?: (item: ThreadItem) => ReactNode;
}
export interface AppPaneProps {
  mode: AppMode;
  density: Density;
  viewer: Viewer;
  binding: ChatBinding;
  collapsed: boolean;
  onCollapsed: (next: boolean) => void;
  /** The shell's own navigation for the `open-view` card action. */
  onOpenView: (view: string) => void;
}

export default function Conversation(props: HomeListProps | AppPaneProps) {
  return props.mode.kind === "home" ? <HomeList {...(props as HomeListProps)} /> : <AppPane {...(props as AppPaneProps)} />;
}

/** Home's rows: the exact `<ol>` Home rendered before, one renderer. */
function HomeList({ items, readOnly, liveAfter, onOpenIssue, onRetry, onDiscard, after }: HomeListProps) {
  return (
    <ol className="space-y-3 min-w-0" aria-label="messages" data-rendered={items.length}>
      {items.map((item) => {
        // Live items (or the operator's pending ones) enter with the
        // rise animation; anything at or below the mount seq is history.
        const live = item.type === "pending" || Number(item.key.slice(1)) > liveAfter;
        return (
          <li key={item.key} className={`min-w-0 space-y-2${live ? " msg-in" : ""}`}>
            <ThreadItemView item={item} readOnly={readOnly} onOpenIssue={onOpenIssue} onRetry={onRetry} onDiscard={onDiscard} />
            {after?.(item)}
          </li>
        );
      })}
    </ol>
  );
}

const itemText = (item: ThreadItem): string | null =>
  item.type === "operator" || item.type === "answer" || item.type === "commentary"
    ? item.entry.text
    : item.type === "pending"
      ? item.pending.text
      : null;

/** The same item with every internal id rewritten (host constant, D4). */
function hidden(item: ThreadItem): ThreadItem {
  switch (item.type) {
    case "operator":
    case "answer":
    case "commentary":
      return { ...item, entry: { ...item.entry, text: hideIds(item.entry.text) } };
    case "pending":
      return { ...item, pending: { ...item.pending, text: hideIds(item.pending.text) } };
    default:
      return item;
  }
}

function AppRow({
  item,
  directive,
  frame,
  mode,
  density,
  readOnly,
  actions,
  onFail,
  onRetry,
  onDiscard,
}: {
  item: ThreadItem;
  directive: Directive | null;
  frame: FrameRow | undefined;
  mode: AppMode;
  density: Density;
  readOnly: boolean;
  actions: ActionContext;
  onFail: (tag: string) => void;
  onRetry: (message: string, text: string, refs?: ThreadRef[]) => void;
  onDiscard: (message: string) => void;
}) {
  if (directive?.kind === "card") return <DirectiveCard card={directive.card} fields={directive.fields} actions={actions} />;
  if (directive?.kind === "confirmation") return <ConfirmationCard />;
  if (directive?.kind === "frame" && frame) {
    if (frame.state === "closed") return <p className="text-micro text-ink-500" data-chat-frame-state="closed">Preview closed</p>;
    if (frame.state === "updated") return <p className="text-micro text-ink-500" data-chat-frame-state="updated">Preview updated above</p>;
    return (
      <ChatFrame
        installId={mode.installId}
        tag={frame.tag}
        size={frame.size}
        directive={{ kind: frame.match, data: frame.data }}
        onFail={onFail}
      />
    );
  }
  // Plain text: a failed or refused directive reads as its message.
  if (item.type === "tools") {
    const steps = toolSteps(item.entries);
    const open = steps.some((s) => !s.done);
    const failed = steps.some((s) => s.error);
    return (
      <p className="text-micro text-ink-500" data-chat-steps>
        {open ? "Working" : failed ? "Finished with an issue" : "✓ Done"} · {steps.length} step{steps.length === 1 ? "" : "s"}
      </p>
    );
  }
  if (item.type === "system" && item.entry.payload?.source !== "permission") {
    return <p className="text-micro text-ink-500">· {hideIds(stepSummary(item.entry.text))}</p>;
  }
  return (
    <ThreadItemView
      item={hidden(item)}
      density={density}
      readOnly={readOnly}
      onOpenIssue={() => undefined}
      onRetry={onRetry}
      onDiscard={onDiscard}
    />
  );
}

/**
 * The install's assistant conversation (CAD-1098): one thread store per
 * conversation, streamed live from `/api/threads/master/stream?conversation=`.
 * The picker lists General, the install's other conversations and + New. Sends
 * carry the route's installation and context; the daemon proves both and
 * stamps the verified binding on the entry.
 */
function AppPane({ mode, density, viewer, binding, collapsed, onCollapsed, onOpenView }: AppPaneProps) {
  const { installId, descriptor } = mode;
  const active = useActiveConversation(installId);
  const store = active.store ?? idleThread;
  const convId = active.state === "ready" ? (active.selected?.id ?? null) : null;
  const thread = useResource(store);
  const masterState = useQuery(resources.masterState);
  const [draft, setDraft] = useState("");
  const [sendError, setSendError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [failedFrames, setFailedFrames] = useState<ReadonlySet<string>>(new Set());
  const loaded = thread.data !== null;
  const draftSubject = active.draftSubject;
  // With no context selected the send still carries the installation, so it
  // lands in the app's conversation (the context is only a per-turn hint).
  const sendApp = binding.scope ?? (active.state === "ready" ? { install_id: installId } : undefined);
  const usable =
    active.state === "legacy" || (active.state === "ready" && (convId !== null || draftSubject !== null));
  const subjects = descriptor?.subjects ?? [];

  useEffect(() => setFailedFrames(new Set()), [installId, convId]);
  useEffect(() => {
    if (usable && !loaded) void store.refresh();
  }, [usable, loaded, store]);
  useEffect(() => {
    if (!usable || !loaded || thread.data?.missing === true || draftSubject !== null) return;
    const sub = streamInto(store, reduceFrame, {
      url: convId === null ? `/api/threads/${MASTER}/stream` : conversationStreamUrl(convId),
      events: ["entry"],
      lastEventId: String(lastSeq(store.get().data) ?? 0),
      onError: () => undefined,
    });
    return () => sub.close();
  }, [store, usable, convId, loaded, thread.data?.missing, draftSubject]);

  const items = threadItems(thread.data);
  const tail = items.slice(-8);
  const canSend = viewer.operator && !viewer.readOnly && usable;
  const canCreate = viewer.operator && !viewer.readOnly && active.state === "ready";
  // Why the composer is disabled — always a plain reason, never a fake reply.
  const sendBlockedReason = !viewer.operator
    ? "Sign in as the operator to message the assistant"
    : viewer.readOnly
      ? "Read-only · Sending is unavailable"
      : active.state === "failed"
        ? "The conversations could not be read — retry above"
        : "No conversation yet — start one with + New";
  // Waiting dot: the selected conversation grew while the rail was collapsed.
  const seen = useRef(items.length);
  if (!collapsed) seen.current = items.length;
  const waiting = collapsed && items.length > seen.current;
  const ctx = chatContext(descriptor, mode.screen, mode.recordOpen);
  const queued = isQueuedBehindOther(masterState.data?.turn, thread.data?.entries ?? [], thread.data?.pending ?? []);
  // The notice needs a live turn read while a reply is awaited.
  const awaiting = queued || (thread.data?.pending ?? []).some((p) => p.state === "sent");
  // After a reload or navigation the selected conversation may still be
  // waiting on another conversation's turn: read the master's state once
  // (the poll below then keeps the notice live while it is queued).
  const unanswered = hasUnansweredOperator(thread.data?.entries ?? []);
  useEffect(() => {
    if (unanswered) void resources.masterState.refresh();
  }, [unanswered, convId]);
  useEffect(() => {
    if (!awaiting) return;
    const t = setInterval(() => void resources.masterState.refresh(), 6_000);
    return () => clearInterval(t);
  }, [awaiting]);

  // "+ New", `/new` and `/clear` are one call: a fresh conversation.
  const startNew = () => {
    if (!canCreate || creating) return;
    setCreating(true);
    setSendError(null);
    createConversation(installId, binding.scope?.context_id ?? "")
      .catch((e: unknown) => setSendError(e instanceof ApiError ? e.message : String(e)))
      .finally(() => setCreating(false));
  };

  /** One delivery of a message to a conversation's store: pending, send, settle. */
  const deliver = useCallback(
    (dest: typeof store, id: string | null, message: string, body: string, refs?: ThreadRef[]) =>
      api
        .threadSend(MASTER, body, message, refs, sendApp, id ?? undefined)
        .then(() => {
          dest.write((s) => settlePending(s, message, { ok: true }));
          void resources.masterState.refresh();
        })
        .catch((e: ApiError) => {
          setSendError(e.message ?? String(e));
          dest.write((s) => settlePending(s, message, { ok: false, error: e.message ?? String(e) }));
        }),
    [sendApp],
  );

  const send = () => {
    const body = draft.trim();
    if (!body || !canSend) return;
    if (parseSlash(body) === "new") {
      // A command, never a message: nothing is sent to the assistant.
      setDraft("");
      startNew();
      return;
    }
    if (binding.error !== null) {
      setSendError(binding.error);
      return;
    }
    const message = newMessageId();
    setSendError(null);
    // Create-on-first-send: an unsaved subject conversation is made
    // (idempotently) right before its first message, then the message
    // goes to the returned id.
    const target: Promise<{ id: string | null; store: typeof store }> =
      draftSubject !== null
        ? createConversation(installId, binding.scope?.context_id ?? "", draftSubject).then((c) => ({
            id: c.id,
            store: conversationThread(c.id),
          }))
        : Promise.resolve({ id: convId, store });
    setDraft("");
    void target.then(
      ({ id, store: dest }) => {
        dest.write((s) => addPending(s, message, body, Date.now()));
        return deliver(dest, id, message, body);
      },
      (e: unknown) => setSendError(e instanceof ApiError ? e.message : String(e)),
    );
  };
  const retry = (message: string, body: string, refs?: ThreadRef[]) => {
    if (!canSend) return;
    store.write((s) => addPending(s, message, body, Date.now(), refs));
    void deliver(store, convId, message, body, refs);
  };
  const discard = (message: string) => store.write((s) => discardPending(s, message));

  const actions = useMemo<ActionContext>(() => ({ openView: onOpenView }), [onOpenView]);
  const directives = tail.map((item) => {
    const text = itemText(item);
    return { key: item.key, directive: text === null ? null : matchDirective(text, descriptor) };
  });
  const plan = planFrames(directives, failedFrames);
  const onFrameFail = useCallback((tag: string) => setFailedFrames((s) => new Set(s).add(tag)), []);
  const readOnly = !viewer.operator || viewer.readOnly;

  const subjectOf = (subject: string | null) => subject?.split(":")[0] ?? null;
  const draftKind = subjectOf(draftSubject);
  const draftLabel = subjects.find((s) => s.kind === draftKind)?.label.toLowerCase();

  return (
    <div className="app-chat" data-chat-pane data-collapsed={collapsed || undefined}>
      <button
        type="button"
        className="btn btn-ghost btn-sm app-chat-rail"
        aria-label="Expand assistant chat"
        aria-expanded={!collapsed}
        onClick={() => onCollapsed(false)}
        data-waiting={waiting || undefined}
      >
        {waiting && <span className="app-chat-dot" role="status" aria-label="New reply waiting" />}
        <span className="app-chat-rail-label text-micro text-ink-400">ASSISTANT</span>
      </button>
      <div className="app-chat-head">
        <p className="app-chat-label slabel">
          Assistant{descriptor?.presentation.showContext && mode.contextName ? ` · ${mode.contextName}` : ""}
        </p>
        <button
          type="button"
          className="btn btn-ghost btn-sm app-chat-collapse"
          aria-label="Collapse assistant chat"
          aria-expanded={!collapsed}
          onClick={() => onCollapsed(true)}
        >
          ⇤
        </button>
      </div>
      {active.state !== "legacy" && (
        <div className="app-chat-conv" data-chat-conversations>
          <select
            className="app-chat-conv-select text-secondary"
            aria-label="Conversation"
            value={convId ?? ""}
            disabled={active.state !== "ready" || (active.conversations.length === 0 && draftSubject === null)}
            onChange={(e) => e.target.value !== "" && selectConversation(installId, e.target.value)}
          >
            {active.conversations.length === 0 && draftSubject === null && <option value="">General</option>}
            {draftSubject !== null && (
              <option value="">{draftLabel ? `New ${draftLabel} conversation (unsaved)` : "New conversation (unsaved)"}</option>
            )}
            {active.conversations.map((c, i) => (
              <option key={c.id} value={c.id}>
                {conversationLabel(c, i, subjects)}
              </option>
            ))}
          </select>
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            data-chat-new
            disabled={!canCreate || creating}
            onClick={startNew}
          >
            + New
          </button>
        </div>
      )}
      {active.state === "failed" && (
        <p className="text-label text-fail" role="alert">
          The conversations could not be read — {active.error}{" "}
          <button type="button" className="lnk" onClick={active.retry}>
            Retry
          </button>
        </p>
      )}
      {active.state === "ready" && convId === null && (
        <p className="text-label text-ink-500" data-empty="conversations">
          No conversation yet. Start one with + New.
        </p>
      )}
      {queued && (
        <p className="text-label text-ink-300" role="status" data-chat-queued>
          {QUEUED_NOTICE}
        </p>
      )}
      {thread.status === "failed" && (
        <p className="text-label text-fail" role="alert">
          The thread could not be read — {thread.error}{" "}
          <button type="button" className="lnk" onClick={() => void store.refresh()}>
            Retry
          </button>
        </p>
      )}
      {usable && !loaded && thread.status !== "failed" && (
        <p className="text-label text-ink-500" role="status">
          Reading the thread…
        </p>
      )}
      {usable && loaded && tail.length === 0 && (
        <p className="text-label text-ink-500" data-empty="chat">
          Nothing here yet. Send the first message.
        </p>
      )}
      <ol className="app-chat-list" aria-label="Recent master messages">
        {tail.map((item, i) => (
          <li key={item.key} className="text-secondary text-ink-300 break-words" data-chat-item>
            <AppRow
              item={item}
              directive={directives[i].directive}
              frame={plan.get(item.key)}
              mode={mode}
              density={density}
              readOnly={readOnly}
              actions={actions}
              onFail={onFrameFail}
              onRetry={retry}
              onDiscard={discard}
            />
          </li>
        ))}
      </ol>
      <AssistantOperations key={`${installId}\u0000${mode.contextId}`} installId={installId} contextId={mode.contextId} canDecide={viewer.operator && !viewer.readOnly} />
      {sendError && (
        <p className="text-label text-fail" role="alert">
          {sendError}
        </p>
      )}
      {canSend &&
        binding.scope !== null &&
        descriptor?.attachments.map((a) => (
          <div key={a.id}>
            {renderCapability(a.id, a.label, {
              scope: { installId: binding.scope!.install_id, contextId: binding.scope!.context_id },
              canWrite: canSend,
              sendIntent: async (intent) => {
                const message = newMessageId();
                try {
                  await api.threadSend(MASTER, JSON.stringify(intent), message, undefined, sendApp, convId ?? undefined);
                  void store.refresh();
                  void resources.masterState.refresh();
                  return null;
                } catch (e: unknown) {
                  return e instanceof ApiError ? e.message : String(e);
                }
              },
            })}
          </div>
        ))}
      <form
        className="app-chat-form"
        onSubmit={(e) => {
          e.preventDefault();
          send();
        }}
      >
        {ctx !== null && mode.promptLayout === "list" && (
          <div className="app-chat-sugg" data-chat-context data-chat-prompts>
            {ctx.prompts.map((p) => (
              <button
                key={p}
                type="button"
                className="app-chat-sugg-row"
                disabled={!canSend}
                onClick={() => setDraft(p)}
              >
                <span>{p}</span>
                <span aria-hidden="true">→</span>
              </button>
            ))}
          </div>
        )}
        {ctx !== null && mode.promptLayout !== "list" && (
          <div className="app-chat-ctx" data-chat-context>
            <span className="app-chat-chip text-micro text-ink-300">
              Context <b className="font-medium text-ink-100">{ctx.label}</b>
            </span>
            {ctx.prompts.map((p) => (
              <button
                key={p}
                type="button"
                className="app-chat-prompt text-micro"
                disabled={!canSend}
                onClick={() => setDraft(p)}
              >
                {p}
              </button>
            ))}
          </div>
        )}
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
          placeholder={canSend ? "Ask the assistant… (Enter sends)" : sendBlockedReason}
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
