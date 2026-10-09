import { chatContext } from "../chatScreen";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type ReactNode,
} from "react";
import { api, ApiError } from "../../../lib/api";
import { resources, threadReader } from "../../../lib/resources";
import { streamInto } from "../../../lib/sse";
import { useQuery, useResource } from "../../../lib/useResource";
import Link from "../../../ui/Link";
import type { ThreadRef } from "../../../lib/types";
import { MASTER } from "../../home/master";
import { ThreadItemView, type Density } from "../../home/ThreadView";
import {
  addPending,
  applyEarlier,
  discardPending,
  fetchEarlier,
  lastSeq,
  newMessageId,
  reduceFrame,
  settlePending,
  stepSummary,
  threadItems,
  toolSteps,
  visibleWindow,
  type ThreadItem,
} from "../../home/thread";
import type { Viewer } from "../../projects/work";
import {
  QUEUED_NOTICE,
  conversationLabel,
  conversationReader,
  conversationStreamUrl,
  conversationThread,
  createConversation,
  hasUnansweredOperator,
  idleThread,
  invalidateConversationSelection,
  isQueuedBehindOther,
  outboxFor,
  parseSlash,
  putOutbox,
  removeOutbox,
  selectConversation,
  subscribeOutbox,
  useActiveConversation,
  type ConversationContextProof,
  type ConversationLinkRequest,
  type OutboxEnvelope,
} from "../conversationClient";
import {
  composerScope,
  moveComposerScope,
  restoreSavedIntent,
  savedIntentsFor,
  setComposerDraft,
  setComposerScope,
  subscribeSavedIntents,
  type ComposerScope,
} from "./composerStore";
import type { ActionContext } from "./actions";
import { renderCapability } from "./capabilities";
import Composer from "./Composer";
import type { AppChat } from "./contract";
import ChatFrame from "./ChatFrame";
import DirectiveCard, { ConfirmationCard } from "./DirectiveCard";
import AssistantOperations from "./AssistantOperations";
import { hideIds, matchDirective, type Directive } from "./directive";
import { planFrames, type FrameRow } from "./frames";
import type { FileUploadProjection } from "./descriptorClient";
import type { AttachDestination } from "./attach";
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
      /** The scope line's text: the real context, else the installed app's name. */
      scopeLabel: string;
      /** The validated descriptor for this install, or null (plain chat). */
      descriptor: AppChat | null;
      /** `list` lays the screen's static prompts out as suggestion rows and
       *  drops the context chip; default is the compact chip row. */
      promptLayout?: "chips" | "list";
      /** A one-shot host URL selector; native list membership proves it. */
      conversationRequest: ConversationLinkRequest | null;
      /** The current host context receipt, compared with a URL scope selector. */
      contextProof?: ConversationContextProof;
      /** Separate live native capability projection, not app-chat schema data. */
      fileUpload: FileUploadProjection;
      fileUploadLoading: boolean;
    };
type AppMode = Extract<ConversationMode, { kind: "app" }>;

/** Items rendered at most in a pane before "Load earlier" pages more —
 *  a compact window under Home's 300-item one. */
const APP_WINDOW = 120;

export interface HomeListProps {
  mode: { kind: "home" };
  items: ThreadItem[];
  readOnly: boolean;
  liveAfter: number;
  onOpenIssue: (id: string) => void;
  onRetry: (
    message: string,
    text: string,
    refs?: ThreadRef[],
    attachments?: { id: string }[],
  ) => void;
  onDiscard: (message: string) => void;
  /** Extra content under a row (Home's plan cards). */
  after?: (item: ThreadItem) => ReactNode;
}
export interface AppPaneProps {
  mode: AppMode;
  density: Density;
  viewer: Viewer;
  binding: ChatBinding;
  /** Refresh the live projection after the native endpoint refuses upload. */
  onFileUploadUnavailable: () => void;
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

/** The reader's anchor: the first list row still visible at the scroller's
 *  top, by item key and its offset inside the viewport. */
function captureAnchor(el: HTMLElement): { key: string; top: number } | null {
  const listTop = el.getBoundingClientRect().top;
  const rows = el.querySelectorAll<HTMLElement>("[data-item-key]");
  for (let i = 0; i < rows.length; i++) {
    const rect = rows[i].getBoundingClientRect();
    if (rect.bottom > listTop) return { key: rows[i].dataset.itemKey ?? "", top: rect.top - listTop };
  }
  return null;
}

/** Put the captured row back at the same viewport offset after content
 *  above it grew. Scroll geometry only — never offsetTop/offsetParent. */
function restoreAnchor(el: HTMLElement, anchor: { key: string; top: number } | null): void {
  if (!anchor || anchor.key === "") return;
  const rows = el.querySelectorAll<HTMLElement>("[data-item-key]");
  let target: HTMLElement | null = null;
  for (let i = 0; i < rows.length; i++) {
    if (rows[i].dataset.itemKey === anchor.key) {
      target = rows[i];
      break;
    }
  }
  if (!target) return;
  const listTop = el.getBoundingClientRect().top;
  const now = target.getBoundingClientRect().top - listTop;
  if (now !== anchor.top) el.scrollTop += now - anchor.top;
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
  onRetry: (
    message: string,
    text: string,
    refs?: ThreadRef[],
    attachments?: { id: string }[],
  ) => void;
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
function AppPane({ mode, density, viewer, binding, onFileUploadUnavailable, collapsed, onCollapsed, onOpenView }: AppPaneProps) {
  const { installId, descriptor } = mode;
  const fileUpload = mode.fileUpload ?? { declared: false, available: false };
  const fileUploadLoading = mode.fileUploadLoading ?? true;
  const active = useActiveConversation(installId, mode.conversationRequest, mode.contextProof);
  const store = active.store ?? idleThread;
  const convId = active.state === "ready" ? (active.selected?.id ?? null) : null;
  const thread = useResource(store);
  const masterState = useQuery(resources.masterState);
  // One draft per conversation: the composer remounts per store, so
  // switching conversations keeps each one's typed text (the stores and
  // their drafts are per-conversation cache entries, never shared).
  const [sendError, setSendError] = useState<string | null>(null);
  // The composer's in-place notices (unsupported slash verb, an
  // unresolved attachment holding the send) — information, not the
  // sendError alert; cleared by the next successful send.
  const [composerNotice, setComposerNotice] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [historyOpen, setHistoryOpen] = useState(false);
  // CAD-1168: like Home's WINDOW — the pane renders a bounded window of
  // the newest items and pages older entries of the SAME conversation
  // via `before`, instead of silently clipping to the newest eight.
  const [limit, setLimit] = useState(APP_WINDOW);
  const [loadingEarlier, setLoadingEarlier] = useState(false);
  // Scroll-follow (mock behaviour 5): new content is followed only while
  // the reader sits at the tail; scrolled up, a jump-to-latest pill
  // appears instead of dragging them down.
  const listRef = useRef<HTMLOListElement>(null);
  const atTail = useRef(true);
  const [jump, setJump] = useState(false);
  // The last tail item seen: the jump pill is driven by a new tail
  // identity — history prepending above the window never raises it.
  const tailKey = useRef<string | null>(null);
  // The reader's anchor for a prepend: captured before older rows are
  // inserted, restored in the layout pass after they render.
  const anchorRef = useRef<{ key: string; top: number } | null>(null);
  // The conversation the pane is showing, as a monotonically increasing
  // visit id: returning to the same conversation (A→B→A) is a NEW visit,
  // so a page that lands from an earlier visit cannot move this one's
  // window, anchor or loading flag.
  const visitRef = useRef(0);
  const [failedFrames, setFailedFrames] = useState<ReadonlySet<string>>(new Set());
  const loaded = thread.data !== null;
  const draftSubject = active.draftSubject;
  const composerKey = `app|${installId}|${convId ?? draftSubject ?? ""}`;
  // The store the pane is showing: a late send error paints only while
  // its own destination is the visible one, never an unrelated pane.
  const storeRef = useRef(store);
  storeRef.current = store;
  // The scope the pane is showing right now, read at callback time: a
  // create failure paints only while the pane still shows the scope the
  // send left from, never a conversation the operator moved to.
  const composerKeyRef = useRef(composerKey);
  composerKeyRef.current = composerKey;
  // CAD-1168: the displayed frame (install, context or a registered
  // absence) is a second identity beside the native conversation key. A
  // frame change invalidates stale selection/alert callbacks exactly like
  // a conversation change: A→B→A is a new frame visit, and a late result
  // from the earlier visit never paints the pane that is showing now.
  let frameScope: ComposerScope | null | undefined = binding.scope;
  if (frameScope === null) {
    if (binding.error !== null || active.state === "loading" || active.state === "failed") {
      frameScope = undefined;
    } else if (active.state === "legacy") {
      frameScope = null;
    } else {
      frameScope = { install_id: installId };
    }
  }
  const frameToken =
    frameScope === undefined
      ? "?"
      : frameScope === null
        ? "-"
        : `${frameScope.install_id}|${frameScope.context_id ?? ""}`;
  const frameRef = useRef(frameToken);
  // The submitted-envelope outbox for THIS key (CAD-1168): one immutable
  // entry per original submission, held until its destination's pending
  // store owns it. A failed create keeps the exact original request here
  // for explicit Retry/Discard, beside any later unsent draft.
  const readOutbox = useCallback(() => outboxFor(composerKey), [composerKey]);
  const outbox = useSyncExternalStore(subscribeOutbox, readOutbox);
  const failedOutbox = outbox !== null && outbox.state === "failed" ? outbox : null;
  // "failed" blocks a new ordinary submission until Retry/Discard;
  // "sending" blocks it until the original settles. The newer text,
  // refs and files stay editable either way.
  const originalUnresolved: "sending" | "failed" | null =
    outbox === null ? null : outbox.state === "failed" ? "failed" : "sending";
  // Newer unsent bundles kept whole because this destination already
  // owned intent: every displaced owner stays reachable here, never
  // hidden behind an unreachable key and never overwritten by a later
  // park.
  const readSaved = useCallback(() => savedIntentsFor(composerKey), [composerKey]);
  const savedIntents = useSyncExternalStore(subscribeSavedIntents, readSaved);
  const outboxRef = useRef<HTMLDivElement>(null);
  // With no context selected the send still carries the installation, so it
  // lands in the app's conversation (the context is only a per-turn hint).
  const sendApp = binding.scope ?? (active.state === "ready" ? { install_id: installId } : undefined);
  // CAD-1168: register this pane's frame for the composer key. The store
  // captures the frame at the intent's first edit (text, refs or a chosen
  // file) and keeps it as the intent's origin, so a restored or moved
  // intent is submitted to the frame that owned it — never to the frame
  // that happens to be current at send time. A displayed frame owns its
  // own live slot: entering a different registered frame parks the old
  // whole bundle under its original owner and exposes a fresh slot, so a
  // new frame's question can never inherit the earlier frame's captured
  // absence. A frame with no app binding registers an explicit absence;
  // a not-yet-resolved frame (loading/failed) is not registered at all,
  // so it can never capture an absence it did not display.
  useLayoutEffect(() => {
    setComposerScope(composerKey, frameScope);
    // Register before the committed composer can accept input. The token
    // captures the whole identity; frameScope is read only inside.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [composerKey, frameToken]);
  const usable =
    (active.state === "legacy" && mode.conversationRequest === null) ||
    (active.state === "ready" && (convId !== null || draftSubject !== null));
  const subjects = descriptor?.subjects ?? [];

  useEffect(() => setFailedFrames(new Set()), [installId, convId]);
  // Switching conversations resets the render window and paging state —
  // the new conversation starts at its own tail.
  useLayoutEffect(() => {
    visitRef.current += 1;
    frameRef.current = frameToken;
    invalidateConversationSelection();
    setSendError(null);
    setComposerNotice(null);
    setCreating(false);
    setLimit(APP_WINDOW);
    setLoadingEarlier(false);
    atTail.current = true;
    setJump(false);
    tailKey.current = null;
    anchorRef.current = null;
  }, [installId, convId, frameToken]);
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
  const { shown, hidden } = visibleWindow(items, limit);
  // A prepend (widened window or a fetched page) keeps the reader's row
  // at the same viewport offset, measured after the DOM updated.
  useLayoutEffect(() => {
    const el = listRef.current;
    const anchor = anchorRef.current;
    anchorRef.current = null;
    if (el && anchor) restoreAnchor(el, anchor);
  }, [items, limit]);
  // `moreBefore` unknown (an old daemon) still lets the button try one
  // backward read; a false answers the listing is complete.
  const moreBefore = thread.data?.moreBefore !== false;
  const onListScroll = () => {
    const el = listRef.current;
    if (!el) return;
    atTail.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
    if (atTail.current) setJump(false);
  };
  // Older history does not change the tail identity and must not be
  // advertised as a new message. Reader-anchor restoration remains a
  // separate installed-app QA requirement.
  useEffect(() => {
    const el = listRef.current;
    if (!el) return;
    const tail = items[items.length - 1]?.key ?? null;
    if (tail === tailKey.current) return;
    if (atTail.current || tailKey.current === null) {
      el.scrollTop = el.scrollHeight;
    } else {
      setJump(true);
    }
    tailKey.current = tail;
  }, [items]);
  const jumpToLatest = () => {
    const el = listRef.current;
    if (el) el.scrollTop = el.scrollHeight;
    atTail.current = true;
    setJump(false);
  };
  const canSend = viewer.operator && !viewer.readOnly && usable;
  const canCreate =
    viewer.operator &&
    !viewer.readOnly &&
    active.state === "ready" &&
    active.requestError === null &&
    !active.requestPending &&
    binding.error === null;
  const capturedBinding = composerScope(composerKey);
  const uploadScope = capturedBinding === undefined ? frameScope : capturedBinding;
  const attachEnabled =
    viewer.operator &&
    !viewer.readOnly &&
    active.state === "ready" &&
    (convId !== null || draftSubject !== null) &&
    binding.error === null &&
    frameScope !== undefined &&
    frameScope !== null &&
    uploadScope !== undefined &&
    uploadScope !== null &&
    fileUpload.declared &&
    fileUpload.available &&
    !fileUploadLoading;
  const uploadOrigin = { key: composerKey, visit: visitRef.current, frame: frameToken };
  const isCurrentUploadOrigin = () =>
    composerKeyRef.current === uploadOrigin.key &&
    visitRef.current === uploadOrigin.visit &&
    frameRef.current === uploadOrigin.frame;
  let attachDestination: AttachDestination | undefined;
  if (attachEnabled && uploadScope !== undefined && uploadScope !== null) {
    const app = {
      install_id: uploadScope.install_id,
      ...(uploadScope.context_id ? { context_id: uploadScope.context_id } : {}),
    };
    if (convId !== null) {
      attachDestination = {
        app,
        conversation: convId,
        refreshCapability: onFileUploadUnavailable,
      };
    } else if (draftSubject !== null) {
      attachDestination = {
        app,
        subject: draftSubject,
        prepareConversation: async (stillOwned) => {
          const created = await createConversation(app.install_id, app.context_id ?? "", draftSubject, {
            shouldSelect: () => stillOwned() && isCurrentUploadOrigin(),
          });
          return created.id;
        },
        refreshCapability: onFileUploadUnavailable,
      };
    }
  }
  const canAttach = attachEnabled && attachDestination !== undefined;
  const attachReason = fileUploadLoading
    ? "Checking this app's file-upload capability…"
    : !fileUpload.declared
      ? "This app has not declared the approved text-file upload capability."
      : !fileUpload.available
        ? "This app's text-file upload capability is currently unavailable."
        : capturedBinding === null
          ? "This saved draft has no app binding; start a new app-scoped draft before attaching."
          : binding.error !== null
            ? binding.error
            : "Attach is available only in a writable, resolved app conversation.";
  // Why the composer is disabled — always a plain reason, never a fake reply.
  const sendBlockedReason = !viewer.operator
    ? "Sign in as the operator to message the assistant"
    : viewer.readOnly
      ? "Read-only · Sending is unavailable"
      : active.state === "failed"
        ? "The conversations could not be read — retry above"
        : "No conversation yet — start one with + New";
  const ctx = chatContext(descriptor, mode.screen, mode.recordOpen);
  // Waiting dot: the selected conversation grew while the rail was collapsed.
  const seen = useRef(items.length);
  if (!collapsed) seen.current = items.length;
  const waiting = collapsed && items.length > seen.current;
  const queued = isQueuedBehindOther(masterState.data?.turn, thread.data?.entries ?? [], thread.data?.pending ?? []);
  // The notice needs a live turn read while a reply is awaited.
  const awaiting = queued || (thread.data?.pending ?? []).some((p) => p.state === "sent");
  // Stop (mock behaviour 5): the same masterState poll that drives the
  // queued notice reports a working turn; `stop` is the board's existing
  // allowlisted command — it cancels the turn, never a committed effect.
  // The board's `stop` is global, so it is offered only when the running
  // turn's message is provably this conversation's own; an unattributable
  // turn keeps the control off the surface rather than mislabeling it.
  const turnMessage = masterState.data?.turn?.message;
  const turnOwned =
    turnMessage !== undefined &&
    ((thread.data?.entries ?? []).some((e) => e.message === turnMessage) ||
      (thread.data?.pending ?? []).some((p) => p.message === turnMessage && p.state !== "failed"));
  const working = masterState.data?.turn?.state === "working" && turnOwned;
  const queuedOwn = masterState.data?.turn?.state === "queued" && turnOwned;
  // Keep feedback across POST acknowledgement; the daemon's proven turn wins.
  const sending = (thread.data?.pending ?? []).some((p) => p.state === "sending");
  const pendingStatus = working || queuedOwn || queued
    ? null
    : sending ? "sending"
      : (thread.data?.pending ?? []).some((p) => p.state === "sent") ? "waiting" : null;
  const onStop = () => {
    const origin = { key: composerKey, visit: visitRef.current, frame: frameToken };
    const isCurrent = () =>
      composerKeyRef.current === origin.key &&
      visitRef.current === origin.visit &&
      frameRef.current === origin.frame;
    void api
      .masterCommand("stop")
      .catch((e: unknown) => {
        if (isCurrent()) setSendError(e instanceof ApiError ? e.message : String(e));
      })
      .finally(() => void resources.masterState.refresh());
  };
  // After a reload or navigation the selected conversation may still be
  // waiting on another conversation's turn: read the master's state once
  // (the poll below then keeps the notice live while it is queued).
  const unanswered = hasUnansweredOperator(thread.data?.entries ?? []);
  useEffect(() => {
    if (unanswered) void resources.masterState.refresh();
  }, [unanswered, convId]);
  useEffect(() => {
    if (!awaiting && !sending && !queuedOwn && !working) return;
    const t = setInterval(() => void resources.masterState.refresh(), 6_000);
    return () => clearInterval(t);
  }, [awaiting, sending, queuedOwn, working]);

  // "+ New", `/new` and `/clear` are one call: a fresh conversation.
  const startNew = () => {
    if (!canCreate || creating) return;
    const origin = { key: composerKey, visit: visitRef.current, frame: frameToken };
    const isCurrent = () =>
      composerKeyRef.current === origin.key &&
      visitRef.current === origin.visit &&
      frameRef.current === origin.frame;
    setCreating(true);
    setSendError(null);
    createConversation(installId, binding.scope?.context_id ?? "", undefined, { shouldSelect: isCurrent })
      .catch((e: unknown) => {
        if (isCurrent()) setSendError(e instanceof ApiError ? e.message : String(e));
      })
      .finally(() => {
        if (isCurrent()) setCreating(false);
      });
  };

  /** One delivery of a message to a conversation's store: pending, send,
   *  settle. `app` is the verified binding the send carried — a retry
   *  resends the original destination, never the current route's scope.
   *  A late failure paints the pane alert only while its own destination
   *  store is the visible one AND the pane is still the visit that sent
   *  it (A→B→A is a new visit). */
  const deliver = useCallback(
    (
      dest: typeof store,
      id: string | null,
      message: string,
      body: string,
      refs?: ThreadRef[],
      attachments?: { id: string }[],
      app?: { install_id: string; context_id?: string },
    ) => {
      const origin = { key: composerKeyRef.current, visit: visitRef.current, frame: frameRef.current };
      return api
        .threadSend(MASTER, body, message, refs, app, id ?? undefined, attachments)
        .then(() => {
          dest.write((s) => settlePending(s, message, { ok: true }));
          void resources.masterState.refresh();
        })
        .catch((e: ApiError) => {
          if (
            storeRef.current === dest &&
            composerKeyRef.current === origin.key &&
            visitRef.current === origin.visit &&
            frameRef.current === origin.frame
          ) {
            setSendError(e.message ?? String(e));
          }
          dest.write((s) => settlePending(s, message, { ok: false, error: e.message ?? String(e) }));
        });
    },
    [],
  );

  /** Adopt one immutable submitted envelope into its destination store:
   *  the pending row carries the same message id, body, refs, file ids
   *  and captured destination, and the send goes to the captured
   *  conversation — never the route's current selection. */
  const adopt = useCallback(
    (dest: typeof store, id: string | null, envelope: OutboxEnvelope) => {
      dest.write((s) =>
        addPending(
          s,
          envelope.message,
          envelope.text,
          envelope.at,
          envelope.refs,
          envelope.attachments,
          envelope.app,
        ),
      );
      void deliver(
        dest,
        id,
        envelope.message,
        envelope.text,
        envelope.refs,
        envelope.attachments,
        envelope.app,
      );
    },
    [deliver],
  );

  /** Create (or open) the subject conversation, then adopt `envelope`
   *  into the returned conversation's own store. Creation uses the
   *  binding CAPTURED on the envelope — installation, context and
   *  subject, including a captured absence — never the route's current
   *  selection. The unsaved-subject scope becomes that conversation, so
   *  newer unsent intent moves with it when the whole bundle fits;
   *  otherwise it stays whole and stays reachable (never newer source
   *  files beside a different destination draft). A failure leaves the
   *  exact envelope recoverable under the original key, and paints this
   *  pane's alert only while this pane is still the visit that sent it. */
  const createAndAdopt = (
    originKey: string,
    create: string,
    envelope: OutboxEnvelope,
    origin: { install: string; visit: number; frame: string },
  ): Promise<void> => {
    const capturedInstall = envelope.app?.install_id ?? origin.install;
    const capturedContext = envelope.app?.context_id ?? "";
    const alert = (text: string) => {
      if (
        composerKeyRef.current === originKey &&
        installId === origin.install &&
        visitRef.current === origin.visit &&
        frameRef.current === origin.frame
      ) {
        setSendError(text);
      }
    };
    /** Record the failure on the original envelope — but only while the
     *  outbox still owns this exact message. A discarded envelope is
     *  never resurrected by its late callback. */
    const recordFailure = (text: string) => {
      const held = outboxFor(originKey);
      if (held === null || held.message !== envelope.message) return;
      putOutbox(originKey, { ...envelope, state: "failed", error: text });
    };
    let creation: ReturnType<typeof createConversation>;
    try {
      creation = createConversation(capturedInstall, capturedContext, create, {
        shouldSelect: () =>
          composerKeyRef.current === originKey &&
          visitRef.current === origin.visit &&
          frameRef.current === origin.frame,
      });
    } catch (e) {
      const text = e instanceof ApiError ? e.message : String(e);
      alert(text);
      recordFailure(text);
      return Promise.reject(e);
    }
    return creation.then(
      (c) => {
        // The operator may have discarded the envelope while creation was
        // in flight: a request with no remaining owner is never adopted
        // or sent.
        const held = outboxFor(originKey);
        if (held === null || held.message !== envelope.message) return;
        const key = `app|${capturedInstall}|${c.id}`;
        moveComposerScope(originKey, key, create);
        adopt(conversationThread(c.id), c.id, envelope);
        // The destination's pending row now owns this exact message;
        // remove only this message's entry, never a different owner's.
        removeOutbox(originKey, envelope.message);
      },
      (e: unknown) => {
        const text = e instanceof ApiError ? e.message : String(e);
        alert(text);
        recordFailure(text);
        throw e;
      },
    );
  };

  const send = (body: string, refs: ThreadRef[], attachments?: { id: string }[]): Promise<void> => {
    if (!canSend) return Promise.reject(new Error("sending is unavailable"));
    if (parseSlash(body) === "new") {
      // A command, never a message: nothing is sent to the assistant.
      startNew();
      return Promise.resolve();
    }
    if (binding.error !== null) {
      setSendError(binding.error);
      return Promise.reject(new Error(binding.error));
    }
    // Handler-side gate, beside the composer's own: an unresolved
    // submitted owner is never replaced by minting a new message.
    const held = outboxFor(composerKey);
    if (held !== null) {
      const text =
        held.state === "failed"
          ? "The previous message was not sent — retry or discard it before sending again"
          : "Still sending the previous message — wait for it to settle";
      setComposerNotice(text);
      return Promise.reject(new Error(text));
    }
    const message = newMessageId();
    setSendError(null);
    setComposerNotice(null);
    const originKey = composerKey;
    // The envelope carries the intent's captured origin: the binding the
    // first edit was made under, including a captured absence. Only an
    // intent with no captured origin at all falls back to the current
    // frame (the narrow window before the pane's registration effect).
    const captured = composerScope(originKey);
    const envelope: OutboxEnvelope = {
      message,
      text: body,
      refs,
      attachments,
      app: captured === undefined ? sendApp : (captured ?? undefined),
      subject: draftSubject ?? undefined,
      at: Date.now(),
      state: "sending",
    };
    // The immutable submitted envelope is owned by this original key
    // BEFORE any asynchronous creation: a failed create leaves this
    // exact request — message id, body, refs, file ids and captured
    // destination — recoverable beside any later unsent draft. The
    // local handoff is accepted at this point: creation runs
    // independently and a future failure is retained by the outbox, not
    // by pretending local ownership failed.
    if (!putOutbox(originKey, envelope)) {
      const text = "The previous message was not sent — retry or discard it before sending again";
      setComposerNotice(text);
      return Promise.reject(new Error(text));
    }
    const create = draftSubject;
    if (create === null) {
      // The destination conversation is already known: its pending row
      // owns the envelope at once, so the outbox is only a transient
      // owner.
      adopt(store, convId, envelope);
      removeOutbox(originKey, message);
      return Promise.resolve();
    }
    const origin = { install: installId, visit: visitRef.current, frame: frameToken };
    void createAndAdopt(originKey, create, envelope, origin).catch(() => undefined);
    return Promise.resolve();
  };

  /** Retry one failed submitted envelope: the same message id, body,
   *  refs, file ids and captured destination — never the current draft
   *  or route. */
  const retryOutbox = (envelope: OutboxEnvelope) => {
    if (!canSend) return;
    const originKey = composerKey;
    setSendError(null);
    if (!putOutbox(originKey, { ...envelope, state: "sending", error: undefined })) return;
    // The captured subject wins over the route's current one: a retry
    // opens the conversation this envelope was submitted for, never a
    // later selection.
    const create = envelope.subject ?? draftSubject;
    if (create === null) {
      adopt(store, convId, envelope);
      removeOutbox(originKey, envelope.message);
      return;
    }
    const origin = { install: installId, visit: visitRef.current, frame: frameToken };
    void createAndAdopt(originKey, create, envelope, origin).catch(() => undefined);
  };
  const discardOutbox = (envelope: OutboxEnvelope) => removeOutbox(composerKey, envelope.message);

  // A retry resends the pending row's immutable envelope — same
  // message id, text, refs and attachment ids — to the same
  // conversation's store. It never reads the composer's current
  // selection or the route's current conversation: the envelope the
  // first send carried is the only legal resend (the daemon's
  // content check refuses anything else).
  const retry = (message: string, body: string, refs?: ThreadRef[], attachments?: { id: string }[]) => {
    if (!canSend) return;
    // The row is rendered by the active store, so `store` is its own.
    const original = store.get().data?.pending.find((p) => p.message === message);
    // A missing pending row means there is no captured envelope to
    // resend: fabricating one from the current route would invent a
    // destination the operator never sent to.
    if (!original) {
      setSendError("This message is no longer pending — it cannot be retried.");
      return;
    }
    // The binding captured on the pending row wins over the current
    // route; a captured ABSENCE (a legacy/no-binding send) stays absent
    // rather than being silently replaced by a later scope.
    const bindingForRetry = original.app;
    store.write((s) => addPending(s, message, body, Date.now(), refs, attachments, bindingForRetry));
    void deliver(store, convId, message, body, refs, attachments, bindingForRetry);
  };
  const discard = (message: string) => store.write((s) => discardPending(s, message));

  // Page older entries of the SAME conversation, like Home's
  // onEarlier: first widen the window over items already held, then
  // fetch the page below the oldest seq and merge it.
  const onEarlier = useCallback(() => {
    const data = store.get().data;
    if (!data) return;
    const hiddenNow = Math.max(0, threadItems(data).length - limit);
    if (hiddenNow > 0) {
      const el = listRef.current;
      if (el) anchorRef.current = captureAnchor(el);
      setLimit((l) => l + APP_WINDOW);
      return;
    }
    const reader =
      active.state === "legacy" ? threadReader(MASTER) : convId !== null ? conversationReader(convId) : null;
    if (reader === null) return;
    const page = fetchEarlier(reader, data);
    if (!page) {
      store.write((cur) => ({ ...(cur ?? data), moreBefore: false }));
      return;
    }
    const token = visitRef.current;
    setLoadingEarlier(true);
    page
      .then((older) => {
        // A page that lands after a switch (or after returning to this
        // same conversation) belongs to an earlier visit: merge it into
        // its own store, but never move the current pane's window,
        // loading flag or anchor.
        if (visitRef.current === token) {
          const el = listRef.current;
          if (el) anchorRef.current = captureAnchor(el);
        }
        store.write((cur) => applyEarlier(cur, older));
        if (visitRef.current === token) setLimit((l) => l + APP_WINDOW);
      })
      .catch(() => undefined)
      .finally(() => {
        if (visitRef.current === token) setLoadingEarlier(false);
      });
  }, [limit, store, active.state, convId]);

  const actions = useMemo<ActionContext>(() => ({ openView: onOpenView }), [onOpenView]);
  const directives = shown.map((item) => {
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
        <div className="app-chat-title">
          <span className="app-chat-mark" aria-hidden>✳</span>
          <strong>Assistant</strong>
        </div>
        <div className="app-chat-head-tools">
          {active.state !== "legacy" && (
            <button
              type="button"
              className="app-chat-iconbtn"
              aria-label="Conversation history"
              title="Conversation history"
              aria-expanded={historyOpen}
              disabled={active.state !== "ready" || active.conversations.length === 0}
              onClick={() => setHistoryOpen((o) => !o)}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M3 11a9 9 0 1 1 3 8M3 4v7h7M12 7v6l4 2" />
              </svg>
            </button>
          )}
          <button
            type="button"
            className="app-chat-iconbtn app-chat-collapse"
            aria-label="Collapse assistant chat"
            title="Collapse assistant chat"
            aria-expanded={!collapsed}
            onClick={() => onCollapsed(true)}
          >
            ⇤
          </button>
        </div>
      </div>
      {historyOpen && active.state === "ready" && (
        <div className="app-chat-history" role="group" aria-label="Conversation history" data-chat-history>
          {active.conversations.map((c, i) => (
            <button
              key={c.id}
              type="button"
              className="app-chat-history-row"
              aria-current={c.id === convId ? "true" : undefined}
              onClick={() => {
                selectConversation(installId, c.id);
                setHistoryOpen(false);
              }}
            >
              {conversationLabel(c, i, subjects)}
            </button>
          ))}
        </div>
      )}
      {active.state !== "legacy" && (
        <div className="app-chat-conv" data-chat-conversations>
          <select
            className="app-chat-conv-select text-secondary"
            aria-label="Conversation"
            value={convId ?? ""}
            disabled={
              active.state !== "ready" ||
              active.requestPending ||
              active.requestError !== null ||
              (active.conversations.length === 0 && draftSubject === null)
            }
            onChange={(e) => e.target.value !== "" && selectConversation(installId, e.target.value)}
          >
            {active.conversations.length === 0 && draftSubject === null && mode.conversationRequest === null && (
              <option value="">General</option>
            )}
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
            className="app-chat-iconbtn"
            data-chat-new
            aria-label="New conversation"
            title="New conversation"
            disabled={!canCreate || creating}
            onClick={startNew}
          >
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M12 5v14M5 12h14" />
            </svg>
          </button>
        </div>
      )}
      <div className="app-chat-scope" data-chat-scope>
        <span className="app-chat-scope-dot" aria-hidden />
        <span className="truncate">{mode.scopeLabel}</span>
        {active.state === "ready" &&
          active.selected !== null &&
          !active.selected.isGeneral &&
          binding.scope !== null &&
          binding.error === null &&
          binding.scope.install_id === installId &&
          binding.scope.context_id !== "" &&
          mode.contextProof?.status === "ready" &&
          mode.contextProof.contextId === binding.scope.context_id && (
            <Link
              href={`/app-installations/${encodeURIComponent(installId)}?ctx=${encodeURIComponent(binding.scope.context_id)}&conversation=${encodeURIComponent(active.selected.id)}`}
              className="lnk app-chat-scope-link"
              aria-label="Link to this conversation"
              title="Shareable link to this conversation"
              data-chat-conversation-link
            >
              Link
            </Link>
          )}
        {active.state === "ready" &&
          active.selected !== null &&
          !active.selected.isGeneral &&
          mode.contextProof?.status === "ready" &&
          (binding.scope === null || binding.scope.context_id === "") && (
            <span
              className="app-chat-scope-link text-ink-500"
              title="A shareable link needs a verified context"
              data-chat-link-unavailable
            >
              Link unavailable
            </span>
          )}
      </div>
      {active.requestPending && (
        <p className="text-label text-ink-500" role="status" data-chat-link-pending>
          Checking the linked conversation and its context…
        </p>
      )}
      {active.requestError !== null && (
        <p className="text-label text-fail" role="alert" data-chat-link-error>
          {active.requestError}
        </p>
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
      {queuedOwn && (
        <p className="text-label text-ink-300" role="status" data-chat-status="queued">
          Queued…
        </p>
      )}
      {pendingStatus && (
        <p className="text-label text-ink-300" role="status" data-chat-status={pendingStatus}>
          {pendingStatus === "sending" ? "Sending…" : "Waiting…"}
        </p>
      )}
      {working && viewer.operator && !viewer.readOnly && (
        <div className="app-chat-working" role="status">
          <span className="app-chat-pulse" aria-hidden />
          <span>Working…</span>
          <button
            type="button"
            className="app-chat-stop"
            onClick={onStop}
            data-chat-stop
            aria-label="Stop the current turn (applies to the whole assistant, not only this conversation)"
            title="Stops the assistant's running turn. The board command is global, not scoped to this conversation, and cancels the turn only — it cannot undo a committed effect."
          >
            Stop
          </button>
        </div>
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
      {usable && loaded && items.length === 0 && (
        <p className="text-label text-ink-500" data-empty="chat">
          Nothing here yet. Send the first message.
        </p>
      )}
      {usable && loaded && items.length > 0 && (hidden > 0 || moreBefore) && (
        <div className="flex justify-center">
          <button
            type="button"
            className="lnk text-label disabled:opacity-50"
            disabled={loadingEarlier}
            onClick={onEarlier}
            data-chat-earlier
          >
            {loadingEarlier ? "Loading…" : "Load earlier messages ↑"}
          </button>
        </div>
      )}
      <ol className="app-chat-list" ref={listRef} onScroll={onListScroll} aria-label="Conversation messages">
        {shown.map((item, i) => (
          <li
            key={item.key}
            className="text-secondary text-ink-300 break-words"
            data-chat-item
            data-item-key={item.key}
          >
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
      {jump && (
        <button type="button" className="lnk text-label app-chat-jump" onClick={jumpToLatest} data-jump-to-latest>
          ↓ New messages
        </button>
      )}
      {sendError && (
        <p className="text-label text-fail" role="alert">
          {sendError}
        </p>
      )}
      {binding.error !== null && binding.error !== sendError && (
        <p className="text-label text-fail" role="alert" data-chat-binding-error>
          {binding.error}
        </p>
      )}
      {composerNotice && (
        <p className="text-micro text-ink-400" role="status" data-chat-notice>
          {composerNotice}
        </p>
      )}
      {failedOutbox !== null && (
        <div className="app-chat-outbox" data-chat-outbox ref={outboxRef}>
          <p className="text-micro text-ink-400">Not sent — the conversation could not be created.</p>
          <p className="text-secondary text-ink-300 break-words">{failedOutbox.text}</p>
          {failedOutbox.error && <p className="text-micro text-ink-500 break-words">{failedOutbox.error}</p>}
          <p className="text-micro">
            <button type="button" className="lnk" onClick={() => retryOutbox(failedOutbox)}>
              Retry
            </button>{" "}
            ·{" "}
            <button type="button" className="lnk" onClick={() => discardOutbox(failedOutbox)}>
              Discard
            </button>
          </p>
        </div>
      )}
      {savedIntents.length > 0 && (
        <div className="app-chat-outbox" data-chat-saved-intent>
          <p className="text-micro text-ink-400">
            {savedIntents.length === 1
              ? "An unsent draft is kept — this conversation already had its own."
              : `${savedIntents.length} unsent drafts are kept — this conversation already had its own.`}
          </p>
          <p className="text-micro">
            {savedIntents.map((record, i) => (
              <span key={record.slot}>
                {i > 0 && " · "}
                <button
                  type="button"
                  className="lnk"
                  onClick={() => {
                    restoreSavedIntent(composerKey, record.slot);
                  }}
                >
                  {savedIntents.length === 1 ? "Restore saved draft" : `Restore saved draft ${i + 1}`}
                </button>
              </span>
            ))}
          </p>
        </div>
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
                  await api.threadSend(
                    MASTER,
                    JSON.stringify(intent),
                    message,
                    undefined,
                    sendApp,
                    convId ?? undefined,
                  );
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
      {/* CAD-1168: the shared composer at compact density. The key
          remounts it per conversation, so each conversation's draft and
          its citation refs (persisted with the same key) are restored and
          no other's leaks in. CAD-1174 keeps the context concept off the
          surface: no default chip or prompt row, and a record chip waits
          for a server-proven reference rather than the route's
          `recordOpen` flag. */}
      {ctx !== null && mode.promptLayout === "list" && (
        <div className="app-chat-sugg" data-chat-context data-chat-prompts>
          {ctx.prompts.map((p) => (
            <button
              key={p}
              type="button"
              className="app-chat-sugg-row"
              disabled={!canSend}
              onClick={() => setComposerDraft(composerKey, p)}
            >
              <span>{p}</span>
              <span aria-hidden="true">→</span>
            </button>
          ))}
        </div>
      )}
      <Composer
        key={composerKey}
        density="compact"
        block={canSend && binding.error === null ? null : "read-only"}
        blockedPlaceholder={binding.error ?? sendBlockedReason}
        storeKey={composerKey}
        onSend={({ body, refs, attachments }) => send(body, refs, attachments)}
        onCommand={(name) => {
          if (name === "new" || name === "clear") startNew();
          else setComposerNotice(`Unsupported command "/${name}" — it was not sent`);
        }}
        onNotice={setComposerNotice}
        attach={{ enabled: canAttach, reason: attachReason, destination: attachDestination }}
        originalUnresolved={originalUnresolved !== null}
        onResolveOriginal={() => outboxRef.current?.scrollIntoView?.({ block: "nearest" })}
        textareaId="app-shell-chat-box"
        ariaLabel="Message to Assistant"
        placeholder="Message Assistant…"
        className="app-chat-form"
      />
    </div>
  );
}
