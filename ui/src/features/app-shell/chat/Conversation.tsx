import { chatContext } from "../chatScreen";
import { useCanvasFeed } from "../../canvas/feed";
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
import { privateSessionGeneration, usingPrivateOperation, withPrivateOperation, type PrivateOperation } from "../../../lib/privateSession";
import { resources, threadReader } from "../../../lib/resources";
import type { SseErrorState } from "../../../lib/sse";
import { useQuery, useResource } from "../../../lib/useResource";
import Link from "../../../ui/Link";
import type { ThreadRef } from "../../../lib/types";
import { MASTER } from "../../home/master";
import { atTail, jumpLabel, tailTop } from "../../home/dock";
import {
  initialFollow,
  motionBehavior,
  onOperatorScrollUp,
  onPill,
  onScrollSettled,
  onSend,
  onTailChange,
  onViewportScroll,
  reducedMotion,
  type Follow,
  type ScrollKind,
} from "../../home/scrollFollow";
import { ThreadItemView, type Density } from "../../home/ThreadView";
import CorrectionReport from "../../wiki/CorrectionReport";
import {
  addPending,
  discardPending,
  newMessageId,
  settlePending,
  stepSummary,
  threadItems,
  visibleWindow,
  type ThreadItem,
} from "../../home/thread";
import type { Viewer } from "../../projects/work";
import { useLocale } from "../../../lib/locale";
import { displayUiNotice, type UiNotice } from "../../../lib/uiNotice";
import {
  conversationLabel,
  conversationReader,
  conversationStreamUrl,
  conversationThread,
  createConversation,
  hasUnansweredOperator,
  idleThread,
  invalidateConversationSelection,
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
  carryEarlyDraft,
  composerHasIntent,
  composerScope,
  moveComposerScope,
  restoreSavedIntent,
  savedIntentsFor,
  setComposerDraft,
  setComposerScope,
  subscribeDraft,
  subscribeSavedIntents,
  type ComposerScope,
} from "./composerStore";
import type { ActionContext } from "./actions";
import { renderCapability } from "./capabilities";
import Composer, { type ComposerAction } from "./Composer";
import type { AppChat } from "./contract";
import ChatFrame from "./ChatFrame";
import DirectiveCard, { ConfirmationCard } from "./DirectiveCard";
import AssistantOperations from "./AssistantOperations";
import { hideIds, matchDirective, type Directive } from "./directive";
import { planFrames, type FrameRow } from "./frames";
import type { FileUploadProjection } from "./descriptorClient";
import type { AttachDestination } from "./attach";
import type { ChatBinding, MediaComposerTarget } from "./types";
import { usePendingPermission } from "./usePendingApproval";
import { turnBarState } from "./turnBarState";
import { TurnBar } from "./TurnBar";
import { isUnread, onSeenChanged, readSeen, writeSeen, type SeenMap } from "./unreadSeen";
import { setAssistantUnread } from "./assistantUnread";
import { setVisibleConversation } from "./chatVisibility";
import { earlierStep, useThreadStream } from "./conversationThread";

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
      /** A one-shot host URL selector; native list membership proves it. */
      conversationRequest: ConversationLinkRequest | null;
      /** The current host context receipt, compared with a URL scope selector. */
      contextProof?: ConversationContextProof;
      /** The verified installation's canonical `/apps/<key>` path — shareable
       *  links emit it. Null (ambiguous/unresolved) means no link renders. */
      appPath?: string | null;
      /** Separate live native capability projection, not app-chat schema data. */
      fileUpload: FileUploadProjection;
      fileUploadLoading: boolean;
    };
type AppMode = Extract<ConversationMode, { kind: "app" }>;

/** Items rendered at most in a pane before "Load earlier" pages more —
 *  a compact window under Home's 300-item one. */
const APP_WINDOW = 120;
/** The composer's textarea id: the starters fill and focus it. */
const COMPOSER_ID = "app-shell-chat-box";

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
  /** CAD-1462: the running turn's activity for this item — a `header`
   *  slot inside the item's own bubble name row, or a standalone
   *  `row` name row placed before/after the item (the mock's `.who` row
   *  for content that has no bubble of its own). */
  activity?: (item: ThreadItem) => ItemActivity | null;
}

/** Where an item's live activity lands. */
export type ItemActivity =
  | { kind: "header"; node: ReactNode }
  | { kind: "row"; node: ReactNode; position: "before" | "after" };
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
  onMediaTarget?: (target: MediaComposerTarget) => () => void;
}

export default function Conversation(props: HomeListProps | AppPaneProps) {
  return props.mode.kind === "home" ? <HomeList {...(props as HomeListProps)} /> : <AppPane {...(props as AppPaneProps)} />;
}

/** Home's rows: the exact `<ol>` Home rendered before, one renderer. */
function HomeList({ items, readOnly, liveAfter, onOpenIssue, onRetry, onDiscard, after, activity }: HomeListProps) {
  const { t } = useLocale();
  return (
    <ol className="space-y-3 min-w-0" role="log" aria-live="polite" aria-relevant="additions" aria-label={t("messages")} data-rendered={items.length}>
      {items.map((item, index) => {
        // Live items (or the operator's pending ones) enter with the
        // rise animation; anything at or below the mount seq is history.
        const live = item.type === "pending" || Number(item.key.slice(1)) > liveAfter;
        const act = activity?.(item) ?? null;
        return (
          <li key={item.key} className={`min-w-0 space-y-2${live ? " msg-in" : ""}`}>
            {act?.kind === "row" && act.position === "before" ? act.node : null}
            <ThreadItemView item={item} readOnly={readOnly} onOpenIssue={onOpenIssue} onRetry={onRetry} onDiscard={onDiscard} activity={act?.kind === "header" ? act.node : undefined} />
            {item.type === "answer" && <CorrectionReport question={visibleQuestion(items, index)} answer={item.entry.text} />}
            {act?.kind === "row" && act.position === "after" ? act.node : null}
            {after?.(item)}
          </li>
        );
      })}
    </ol>
  );
}

function visibleQuestion(items: ThreadItem[], index: number): string {
  for (let cursor = index - 1; cursor >= 0; cursor--) {
    const item = items[cursor];
    if (item.type === "operator") return item.entry.text;
    if (item.type === "pending") return item.pending.text;
  }
  return "";
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
    case "tools":
      return { ...item, entries: item.entries.map((e) => ({ ...e, text: hideIds(e.text) })) };
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
  question = "",
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
  question?: string;
}) {
  const { t } = useLocale();
  if (directive?.kind === "card") return <DirectiveCard card={directive.card} fields={directive.fields} actions={actions} />;
  if (directive?.kind === "confirmation") return <ConfirmationCard />;
  if (directive?.kind === "frame" && frame) {
    if (frame.state === "closed") return <p className="text-micro text-ink-500" data-chat-frame-state="closed">{t("Preview closed")}</p>;
    if (frame.state === "updated") return <p className="text-micro text-ink-500" data-chat-frame-state="updated">{t("Preview updated above")}</p>;
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
  if (item.type === "system" && item.entry.payload?.source !== "permission") {
    return <p className="text-micro text-ink-500">· {hideIds(stepSummary(item.entry.text))}</p>;
  }
  const shown = hidden(item);
  return (
    <>
      <ThreadItemView
        item={shown}
        density={density}
        readOnly={readOnly}
        onRetry={onRetry}
        onDiscard={onDiscard}
      />
      {shown.type === "answer" && <CorrectionReport question={hideIds(question)} answer={shown.entry.text} />}
    </>
  );
}

/**
 * The install's assistant conversation (CAD-1098): one thread store per
 * conversation, streamed live from `/api/threads/master/stream?conversation=`.
 * The picker lists General, the install's other conversations and + New. Sends
 * carry the route's installation and context; the daemon proves both and
 * stamps the verified binding on the entry.
 */
function AppPane({ mode, density, viewer, binding, onFileUploadUnavailable, collapsed, onCollapsed, onOpenView, onMediaTarget }: AppPaneProps) {
  const { t, formatNumber } = useLocale();
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
  const [composerNotice, setComposerNotice] = useState<UiNotice | null>(null);
  const [creating, setCreating] = useState(false);
  // The thread stream's link: down means the turn bar shows its last known state.
  const [streamLink, setStreamLink] = useState<"live" | SseErrorState>("live");
  const streamDown = streamLink !== "live";
  const [historyOpen, setHistoryOpen] = useState(false);
  // Client-side last-seen per conversation (CAD-1700). Opening a conversation
  // records its newest `updated`; another tab's write arrives via `storage`.
  const [unreadSeen, setUnreadSeen] = useState<SeenMap>(() => readSeen(installId));
  useEffect(() => {
    setUnreadSeen(readSeen(installId));
    return onSeenChanged(installId, setUnreadSeen);
  }, [installId]);
  const selectedId = active.state === "ready" ? (active.selected?.id ?? null) : null;
  const selectedUpdated = active.state === "ready" ? (active.selected?.updated ?? null) : null;
  useEffect(() => {
    if (selectedId === null || selectedUpdated === null) return;
    writeSeen(installId, selectedId, selectedUpdated);
    setUnreadSeen(readSeen(installId));
  }, [selectedId, selectedUpdated, installId]);
  // Any conversation other than the open one has a newer entry than this
  // browser has seen: the Conversations button wears the accent dot.
  const hasUnreadOther =
    active.state === "ready" && active.conversations.some((c) => c.id !== convId && isUnread(unreadSeen, c.id, c.updated));
  useEffect(() => setAssistantUnread(installId, hasUnreadOther), [installId, hasUnreadOther]);
  const historyButtonRef = useRef<HTMLButtonElement | null>(null);
  const historyMenuRef = useRef<HTMLDivElement | null>(null);
  // Escape and an outside press close the Conversations menu; focus goes
  // back to its button so keyboard users keep their place.
  const closeHistory = useCallback(() => {
    setHistoryOpen(false);
    historyButtonRef.current?.focus();
  }, []);
  useEffect(() => {
    if (!historyOpen) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") closeHistory();
    };
    const onPress = (event: PointerEvent) => {
      const target = event.target as Node | null;
      if (target && (historyMenuRef.current?.contains(target) || historyButtonRef.current?.contains(target))) return;
      setHistoryOpen(false);
    };
    document.addEventListener("keydown", onKey);
    document.addEventListener("pointerdown", onPress);
    return () => {
      document.removeEventListener("keydown", onKey);
      document.removeEventListener("pointerdown", onPress);
    };
  }, [historyOpen, closeHistory]);
  // CAD-1431: the CSV importer mounts only when the operator first opens it
  // from the composer's More actions menu. Once mounted it is hidden, not
  // unmounted, on Close, so a confirm already in flight still hands its
  // receipt to the assistant instead of stranding the plan.
  const [csvMounted, setCsvMounted] = useState(false);
  const [csvOpen, setCsvOpen] = useState(false);
  // CAD-1168: like Home's WINDOW — the pane renders a bounded window of
  // the newest items and pages older entries of the SAME conversation
  // via `before`, instead of silently clipping to the newest eight.
  const [limit, setLimit] = useState(APP_WINDOW);
  const [loadingEarlier, setLoadingEarlier] = useState(false);
  // Scroll-follow (CAD-1533): the pane runs the same pin/release/settle
  // model as Home — the shared predicates in features/home/scrollFollow
  // and dock.ts — instead of its own key-only copy. New rows are followed
  // while the reader sits at the tail; scrolled up, a counted jump pill
  // appears instead of dragging them down, and growth (a reply that
  // grows, a block below the list that shrinks the scroller) keeps the
  // newest line in view instead of hiding it.
  const listRef = useRef<HTMLOListElement>(null);
  const follow = useRef<Follow>(initialFollow());
  const [unseen, setUnseen] = useState(0);
  // Follow bookkeeping (CAD-1533 spec/security round 5): `visited` is the
  // one-shot that means "this visit has not landed at the tail yet" — it is
  // NOT the same as "no stored row exists", which is why a pending-only
  // conversation used to jump on every change. `seenStoredSeq` is the
  // highest stored `e<seq>` counted so far, so arrivals are counted by
  // sequence rather than by net list length (a retiring pending row made
  // the net delta undercount, and a prepend made it overcount).
  const visited = useRef(false);
  const seenStoredSeq = useRef(0);
  const prevLen = useRef(0);
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
  // CAD-1279: text typed while the conversation was still resolving lives
  // under the empty-identity key; carry it to the resolved key.
  const prevComposerKey = useRef(composerKey);
  useLayoutEffect(() => {
    const prev = prevComposerKey.current;
    prevComposerKey.current = composerKey;
    if (prev.startsWith(`app|${installId}|`) && prev.endsWith("|")) carryEarlyDraft(prev, composerKey);
  }, [composerKey, installId]);
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
  // Whether the composer holds any unsent text, refs or files, read live so
  // the empty-state starters hide the moment the operator starts typing.
  const subscribeIntent = useCallback((listener: () => void) => subscribeDraft(composerKey, listener), [composerKey]);
  const readIntent = useCallback(() => composerHasIntent(composerKey), [composerKey]);
  const drafting = useSyncExternalStore(subscribeIntent, readIntent);
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
  const subjects = useMemo(() => descriptor?.subjects ?? [], [descriptor]);
  // The shell's announcer reads this: the picker's selection is client state,
  // so the route alone cannot say whether a reply is already on screen.
  useEffect(() => {
    setVisibleConversation(installId, convId);
    return () => setVisibleConversation(installId, null);
  }, [installId, convId]);

  useEffect(() => setFailedFrames(new Set()), [installId, convId]);
  // A context or installation change drops the importer (its scope key
  // also remounts it). A conversation switch keeps it, as before, so a
  // confirm in flight still hands its receipt to the assistant.
  useEffect(() => {
    setCsvOpen(false);
    setCsvMounted(false);
  }, [installId, frameToken]);
  // Switching conversations resets the render window and paging state —
  // the new conversation starts at its own tail.
  useLayoutEffect(() => {
    visitRef.current += 1;
    frameRef.current = frameToken;
    invalidateConversationSelection();
    setSendError(null);
    setComposerNotice(null);
    setCreating(false);
    setStreamLink("live");
    setLimit(APP_WINDOW);
    setLoadingEarlier(false);
    follow.current = initialFollow();
    setUnseen(0);
    visited.current = false;
    seenStoredSeq.current = 0;
    prevLen.current = 0;
    anchorRef.current = null;
  }, [installId, convId, frameToken]);
  useEffect(() => {
    if (usable && !loaded) void store.refresh();
  }, [usable, loaded, store]);
  useThreadStream(
    store,
    convId === null ? `/api/threads/${MASTER}/stream` : conversationStreamUrl(convId),
    usable && loaded && thread.data?.missing !== true && draftSubject === null,
    setStreamLink,
  );

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
  const scrollPanel = useCallback((kind: ScrollKind) => {
    const el = listRef.current;
    if (!el) return;
    el.scrollTo({
      top: tailTop(el.scrollHeight, el.clientHeight),
      behavior: motionBehavior(reducedMotion(), kind),
    });
  }, []);
  const applyFollow = useCallback((next: Follow) => {
    const prev = follow.current.unseen;
    follow.current = next;
    if (next.unseen !== prev) setUnseen(next.unseen);
  }, []);
  // A send pins before the pending row paints (CAD-610's rule, shared
  // with Home): the row that lands right after is then followed. A glide
  // that cannot move fires no `scrollend`, so the lock would stay set and
  // swallow every later follow — already at the tail means there is
  // nothing to glide to (CAD-1533 review, finding 4; Home's own guard).
  const engage = useCallback(() => {
    const next = onSend(follow.current);
    follow.current = next.state;
    setUnseen(0);
    const el = listRef.current;
    if (el && Math.abs(el.scrollTop - tailTop(el.scrollHeight, el.clientHeight)) <= 1) {
      follow.current = { ...follow.current, pinned: true, programmatic: false };
      return;
    }
    scrollPanel(next.scroll);
  }, [scrollPanel]);
  const onListScroll = () => {
    const el = listRef.current;
    if (!el) return;
    applyFollow(onViewportScroll(follow.current, atTail(el.scrollTop, el.scrollHeight, el.clientHeight)));
  };
  // A gesture that moves the reader up releases the pin, so the rest of
  // the turn cannot yank the view back down.
  useEffect(() => {
    const el = listRef.current;
    if (!el) return;
    const leave = () => {
      if (!follow.current.programmatic) return;
      const next = onOperatorScrollUp(follow.current);
      if (next === follow.current) return;
      applyFollow(next);
      el.scrollTo({ top: el.scrollTop, behavior: "auto" });
    };
    const onWheel = (e: WheelEvent) => {
      if (e.deltaY < 0) leave();
    };
    let touchY = 0;
    const onTouchStart = (e: TouchEvent) => {
      touchY = e.touches[0]?.clientY ?? 0;
    };
    const onTouchMove = (e: TouchEvent) => {
      const y = e.touches[0]?.clientY ?? touchY;
      if (y > touchY + 12) leave();
      touchY = y;
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "ArrowUp" && e.key !== "PageUp" && e.key !== "Home") return;
      const target = e.target;
      if (target instanceof Element && target.closest("textarea, input, [contenteditable='true']")) return;
      leave();
    };
    const onScrollEnd = () => {
      const settled = onScrollSettled(
        follow.current,
        atTail(el.scrollTop, el.scrollHeight, el.clientHeight),
      );
      applyFollow(settled.state);
      if (settled.scroll === "jump") scrollPanel("jump");
    };
    el.addEventListener("scrollend", onScrollEnd);
    el.addEventListener("wheel", onWheel, { passive: true });
    el.addEventListener("touchstart", onTouchStart, { passive: true });
    el.addEventListener("touchmove", onTouchMove, { passive: true });
    addEventListener("keydown", onKey);
    return () => {
      el.removeEventListener("scrollend", onScrollEnd);
      el.removeEventListener("wheel", onWheel);
      el.removeEventListener("touchstart", onTouchStart);
      el.removeEventListener("touchmove", onTouchMove);
      removeEventListener("keydown", onKey);
    };
  }, [applyFollow, scrollPanel]);
  // Growth in place: a reply row that grows, a tool group opening, or one
  // of the pane's own blocks below the list — the CSV import panel, a
  // notice, the outbox block — shrinking this scroller. The pane had no
  // observer at all, so those blocks used to leave the newest line
  // hidden behind them (CAD-1533).
  useEffect(() => {
    const el = listRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    let lastHeight = el.scrollHeight;
    let lastBox = el.clientHeight;
    const ro = new ResizeObserver(() => {
      const height = el.scrollHeight;
      const box = el.clientHeight;
      const grew = height > lastHeight || box < lastBox;
      lastHeight = height;
      lastBox = box;
      if (!grew || follow.current.programmatic) return;
      if (follow.current.pinned) scrollPanel("jump");
    });
    // The scroller's own box catches a sibling below it growing; each
    // child catches content growing inside an unchanged row.
    ro.observe(el);
    for (const child of Array.from(el.children)) ro.observe(child);
    return () => ro.disconnect();
  }, [scrollPanel, items.length]);
  // Older history does not change the tail identity and must not be
  // advertised as a new message. Reader-anchor restoration remains a
  // separate installed-app QA requirement.
  useEffect(() => {
    const el = listRef.current;
    if (!el) return;
    // Stored rows are `e<seq>` in ascending order and pending rows trail
    // them, so a retained failed pending row never owns the arrival anchor.
    let storedSeq = 0;
    for (let i = items.length - 1; i >= 0; i--) {
      if (items[i].type === "pending") continue;
      const seq = Number(items[i].key.slice(1));
      if (Number.isFinite(seq)) storedSeq = seq;
      break;
    }
    // Arrivals are counted by SEQUENCE, not by net list length: a batch can
    // retire the operator's pending row while adding three stored ones (net
    // +2), and a history prepend adds rows with older sequences (net +20).
    // The delta lies in both directions; the sequence count does not
    // (CAD-1533 spec/security round 5).
    let arrivals = 0;
    for (let i = items.length - 1; i >= 0; i--) {
      const row = items[i];
      if (row.type === "pending") continue;
      const seq = Number(row.key.slice(1));
      if (!Number.isFinite(seq) || seq <= seenStoredSeq.current) break;
      arrivals += 1;
    }
    const grew = items.length > prevLen.current;
    prevLen.current = items.length;
    if (storedSeq > seenStoredSeq.current) seenStoredSeq.current = storedSeq;
    // The first row-set of a visit lands at the tail. This is a one-shot —
    // a conversation with no stored rows yet still has a pending row to
    // follow and arrivals to count, which is why it is not keyed on "no
    // stored row exists" (CAD-1533 spec/security round 5).
    if (!visited.current) {
      if (items.length === 0) return;
      visited.current = true;
      scrollPanel("jump");
      return;
    }
    if (arrivals === 0) {
      // A pending-only change is never an unseen arrival — the operator's
      // own message is not "new" — but while pinned its row still has to
      // stay in view, and the early return used to skip exactly that
      // (CAD-1533 spec/security round 4, blocking). The resize observer is
      // no substitute: it re-baselines from the already-updated DOM.
      if (grew && follow.current.pinned && !follow.current.programmatic) scrollPanel("jump");
      return;
    }
    const next = onTailChange(follow.current, arrivals);
    applyFollow(next.state);
    if (next.scroll === "jump") scrollPanel("jump");
  }, [items, applyFollow, scrollPanel]);
  const jumpToLatest = () => {
    const next = onPill(follow.current);
    applyFollow(next.state);
    scrollPanel(next.scroll);
  };
  const canSend = viewer.operator === true && !viewer.readOnly && usable;
  const canCreate =
    viewer.operator === true &&
    !viewer.readOnly &&
    active.state === "ready" &&
    active.requestError === null &&
    !active.requestPending &&
    binding.error === null;
  const mediaTargetReady = canCreate && usable;
  const mediaInstallId = binding.scope?.install_id ?? null;
  const mediaContextId = binding.scope?.context_id ?? null;
  useLayoutEffect(() => {
    if (!mediaTargetReady || !mediaInstallId || !mediaContextId || !onMediaTarget) return;
    return onMediaTarget({
      composerKey,
      conversationId: convId,
      scope: { install_id: mediaInstallId, context_id: mediaContextId },
    });
  }, [onMediaTarget, mediaTargetReady, composerKey, convId, mediaInstallId, mediaContextId]);
  const capturedBinding = composerScope(composerKey);
  const uploadScope = capturedBinding === undefined ? frameScope : capturedBinding;
  const attachEnabled =
    viewer.operator === true &&
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
          if (!stillOwned()) throw new DOMException("upload canceled", "AbortError");
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
  const sendBlockedReason = viewer.operator === null
    ? viewer.access === "unavailable"
      ? "Access could not be confirmed — retry the access check"
      : "Checking access before messaging the assistant"
    : viewer.operator === false
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
  // The turn bar (turnBar.ts, shared with Home): Stop carries this
  // conversation's own running message; Cancel its own queued message.
  const entries = thread.data?.entries ?? [];
  const pendingMessages = (thread.data?.pending ?? []).filter((p) => p.state !== "failed").map((p) => p.message);
  const permissionPending = usePendingPermission(entries);
  const [operationPending, setOperationPending] = useState(false);
  // Results older than this pane's view of the conversation are history.
  const liveSince = useMemo(() => Date.now(), [convId, installId]);
  const turnState = turnBarState(
    {
      master: masterState.data,
      entries,
      pendingMessages,
      awaitingApproval: permissionPending || operationPending,
      reconnecting: streamDown,
      liveSince,
      conversationName: (id) => {
        if (active.state !== "ready") return null;
        const index = active.conversations.findIndex((c) => c.id === id);
        return index < 0 ? null : conversationLabel(active.conversations[index], index, subjects, t);
      },
    },
    convId,
  );
  useCanvasFeed({
    conversation: convId,
    surface: "app",
    composerKey,
    items,
    loaded: loaded && thread.data?.missing !== true,
    running:
      turnState.kind === "working" || turnState.kind === "writing" || turnState.kind === "approval"
        ? (masterState.data?.turn?.message ?? null)
        : null,
  });
  // Retry resends this conversation's own failed operator message; with no
  // captured pending row the bar rebuilds one from the failed turn's own
  // operator entry (same text, a fresh message id).
  const failedSend = (thread.data?.pending ?? []).find((p) => p.state === "failed") ?? null;
  const failedEntry =
    turnState.kind === "failed" && turnState.message !== null
      ? entries.find((e) => e.message === turnState.message && e.role === "operator") ?? null
      : null;
  const retryAction =
    failedSend !== null
      ? () => retry(failedSend.message, failedSend.text)
      : failedEntry !== null
        ? () => {
            const payload = failedEntry.payload ?? {};
            const listOf = (key: string) => (Array.isArray(payload[key]) ? (payload[key] as unknown[]) : []);
            const objects = <T,>(raw: unknown[], pick: (item: Record<string, unknown>) => T | null) =>
              raw.flatMap((item) => {
                const picked = item && typeof item === "object" ? pick(item as Record<string, unknown>) : null;
                return picked === null ? [] : [picked];
              });
            const refs = objects(listOf("refs"), (r) =>
              typeof r.kind === "string" && typeof r.id === "string" ? { kind: r.kind, id: r.id } : null,
            );
            const attachments = objects(listOf("attachments"), (a) => (typeof a.id === "string" ? { id: a.id } : null));
            const mediaRefs = objects(listOf("media_refs"), (m) =>
              typeof m.asset_id === "string" && typeof m.revision === "number" && typeof m.sha256 === "string"
                ? { asset_id: m.asset_id, revision: m.revision, sha256: m.sha256 }
                : null,
            );
            void send(failedEntry.text, refs, attachments.length ? attachments : undefined, mediaRefs.length ? mediaRefs : undefined);
          }
        : undefined;
  const turnCommand = (command: "stop" | "cancel", message: string) => {
    const origin = { key: composerKey, visit: visitRef.current, frame: frameToken };
    void api
      .masterCommand(command, undefined, undefined, message)
      .catch((e: unknown) => {
        const current =
          composerKeyRef.current === origin.key && visitRef.current === origin.visit && frameRef.current === origin.frame;
        if (current) setSendError(e instanceof ApiError ? e.message : String(e));
      })
      .finally(() => void resources.masterState.refresh());
  };
  const turnActions = {
    onStop: (message: string) => turnCommand("stop", message),
    onCancel: (message: string) => turnCommand("cancel", message),
    onRetry: retryAction,
    onView:
      turnState.kind !== "queued"
        ? undefined
        : turnState.aheadConversation === null
          ? () => window.location.assign("/")
          : active.state === "ready" && active.conversations.some((c) => c.id === turnState.aheadConversation)
            ? () => selectConversation(installId, turnState.aheadConversation!)
            : (() => {
                // Another install's conversation: the running turn's verified
                // app origin is the one host-resolved way to open it.
                const turn = masterState.data?.turn ?? null;
                const origin = turn?.app_origin ?? null;
                if (origin === null || turn?.conversation !== turnState.aheadConversation) return undefined;
                const href = `/app-installations/${encodeURIComponent(origin.install_id)}?conversation=${encodeURIComponent(turnState.aheadConversation)}`;
                return () => window.location.assign(href);
              })(),
    onReview: () => {
      const card = listRef.current?.querySelector<HTMLElement>("[data-permission-thread], .app-assistant-activity");
      (card ?? listRef.current)?.scrollIntoView({ block: "center", behavior: motionBehavior(reducedMotion(), "jump") });
    },
  };
  // After a reload or navigation the selected conversation may still be
  // waiting on another conversation's turn: read the master's state once
  // (the `master` stream frame keeps it live while it is queued).
  const unanswered = hasUnansweredOperator(thread.data?.entries ?? []);
  useEffect(() => {
    if (unanswered) void resources.masterState.refresh();
  }, [unanswered, convId]);

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
      operation?: PrivateOperation,
      mediaRefs?: { asset_id: string; revision: number; sha256: string }[],
    ) => {
      const origin = { key: composerKeyRef.current, visit: visitRef.current, frame: frameRef.current };
      const ownedAt = dest.get().data?.pending.find((row) => row.message === message)?.at;
      return usingPrivateOperation(operation, async (op) => {
        const releaseAbandoned = () => {
          if (op.generation !== privateSessionGeneration()) return;
          const pending = dest.get().data?.pending.find((row) => row.message === message);
          if (!pending || pending.at !== ownedAt || pending.state !== "sending") return;
          dest.write((s) => settlePending(s, message, { ok: false, error: "The private session changed" }));
        };
        const onAbort = () => releaseAbandoned();
        op.signal.addEventListener("abort", onAbort);
        try {
          await api.threadSend(MASTER, body, message, refs, app, id ?? undefined, attachments, op, mediaRefs);
          op.assertCurrent();
          dest.write((s) => settlePending(s, message, { ok: true }));
          void resources.masterState.refresh();
        } catch (e: unknown) {
          if (op.signal.aborted || op.generation !== privateSessionGeneration()) {
            releaseAbandoned();
            return;
          }
          const error = e instanceof Error ? (e.message || "The private session changed") : String(e);
          op.assertCurrent();
          if (
            storeRef.current === dest &&
            composerKeyRef.current === origin.key &&
            visitRef.current === origin.visit &&
            frameRef.current === origin.frame
          ) {
            setSendError(error);
          }
          dest.write((s) => settlePending(s, message, { ok: false, error }));
        } finally {
          op.signal.removeEventListener("abort", onAbort);
        }
      }).catch((e: unknown) => {
        if (e instanceof DOMException && e.name === "AbortError") return;
        const error = e instanceof Error ? (e.message || "The private session changed") : String(e);
        const pending = dest.get().data?.pending.find((row) => row.message === message);
        if (!pending || pending.at !== ownedAt || pending.state !== "sending") return;
        dest.write((s) => settlePending(s, message, { ok: false, error }));
      });
    },
    [],
  );

  /** Adopt one immutable submitted envelope into its destination store:
   *  the pending row carries the same message id, body, refs, file ids
   *  and captured destination, and the send goes to the captured
   *  conversation — never the route's current selection. */
  const adopt = useCallback(
    (dest: typeof store, id: string | null, envelope: OutboxEnvelope, operation?: PrivateOperation) => {
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
      return deliver(
        dest,
        id,
        envelope.message,
        envelope.text,
        envelope.refs,
        envelope.attachments,
        envelope.app,
        operation,
        envelope.mediaRefs,
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
    /** Record the failure on this envelope object only. A retry, discard,
     *  or reset replaces that object, so a late callback cannot clear it. */
    const recordFailure = (text: string) => {
      if (outboxFor(originKey) !== envelope) return;
      putOutbox(originKey, { ...envelope, state: "failed", error: text });
    };
    return usingPrivateOperation(undefined, async (op) => {
      const onAbort = () => {
        if (op.generation !== privateSessionGeneration()) return;
        recordFailure("The private session changed");
      };
      op.signal.addEventListener("abort", onAbort);
      try {
        const created = await createConversation(capturedInstall, capturedContext, create, {
          shouldSelect: () =>
            composerKeyRef.current === originKey &&
            visitRef.current === origin.visit &&
            frameRef.current === origin.frame,
          operation: op,
        });
        op.assertCurrent();
        // The operator may have discarded the envelope while creation was
        // in flight: a request with no remaining owner is never adopted
        // or sent.
        const held = outboxFor(originKey);
        if (held === null || held.message !== envelope.message) return;
        const key = `app|${capturedInstall}|${created.id}`;
        moveComposerScope(originKey, key, create);
        const sent = adopt(conversationThread(created.id), created.id, envelope, op);
        // The destination's pending row now owns this exact message;
        // remove only this message's entry, never a different owner's.
        removeOutbox(originKey, envelope.message);
        await sent;
      } catch (e) {
        if (op.signal.aborted || op.generation !== privateSessionGeneration()) {
          onAbort();
          throw e;
        }
        const text = e instanceof ApiError ? e.message : e instanceof Error ? e.message : String(e);
        alert(text);
        recordFailure(text);
        throw e;
      } finally {
        op.signal.removeEventListener("abort", onAbort);
      }
    }).catch((e: unknown) => {
      if (e instanceof DOMException && e.name === "AbortError") return;
      const text = e instanceof Error ? e.message : String(e);
      recordFailure(text);
      throw e;
    });
  };

  const send = (body: string, refs: ThreadRef[], attachments?: { id: string }[], mediaRefs?: { asset_id: string; revision: number; sha256: string }[]): Promise<void> => {
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
      mediaRefs,
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
    const attempt: OutboxEnvelope = { ...envelope, state: "sending", error: undefined };
    if (!putOutbox(originKey, attempt)) return;
    // The captured subject wins over the route's current one: a retry
    // opens the conversation this envelope was submitted for, never a
    // later selection.
    const create = envelope.subject ?? draftSubject;
    if (create === null) {
      adopt(store, convId, attempt);
      removeOutbox(originKey, envelope.message);
      return;
    }
    const origin = { install: installId, visit: visitRef.current, frame: frameToken };
    void createAndAdopt(originKey, create, attempt, origin).catch(() => undefined);
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
    const reader =
      active.state === "legacy" ? threadReader(MASTER) : convId !== null ? conversationReader(convId) : null;
    if (reader === null) return;
    const step = earlierStep(store, reader, limit);
    if (step === null || step.kind === "complete") return;
    if (step.kind === "widen") {
      const el = listRef.current;
      if (el) anchorRef.current = captureAnchor(el);
      setLimit((l) => l + APP_WINDOW);
      return;
    }
    const token = visitRef.current;
    setLoadingEarlier(true);
    step.page
      .then((older) => {
        // A page that lands after a switch (or after returning to this
        // same conversation) belongs to an earlier visit: merge it into
        // its own store, but never move the current pane's window,
        // loading flag or anchor.
        if (visitRef.current === token) {
          const el = listRef.current;
          if (el) anchorRef.current = captureAnchor(el);
        }
        step.land(older);
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
  // The CSV importer exists only when the descriptor declares it and the
  // viewer can send through this route's scope.
  const csvAvailable = canSend && binding.scope !== null && descriptor?.attachments.some((a) => a.id === "csv-import") === true;
  const csvActions: ComposerAction[] = csvAvailable
    ? [
        {
          label: t("Import customer CSV…"),
          onSelect: () => {
            setCsvMounted(true);
            setCsvOpen(true);
          },
        },
      ]
    : [];
  // Starters: a loaded, truly empty conversation with nothing unsent, no
  // unresolved or saved submission, and a sendable composer. Loading,
  // failed, stale and read-only states advertise no executable action.
  const starters =
    canSend &&
    binding.error === null &&
    thread.status === "ok" &&
    thread.data?.missing !== true &&
    items.length === 0 &&
    outbox === null &&
    savedIntents.length === 0 &&
    !drafting;

  const subjectOf = (subject: string | null) => subject?.split(":")[0] ?? null;
  const draftKind = subjectOf(draftSubject);
  const draftLabel = subjects.find((s) => s.kind === draftKind)?.label.toLowerCase();
  // The removed picker was the only place naming the open conversation.
  const selectedIndex = active.state === "ready" ? active.conversations.findIndex((c) => c.id === convId) : -1;
  const convLabel =
    active.state !== "ready"
      ? null
      : draftSubject !== null
        ? draftLabel
          ? `New ${draftLabel} conversation (unsaved)`
          : "New conversation (unsaved)"
        : active.selected !== null
          ? conversationLabel(active.selected, selectedIndex, subjects, t)
          : null;

  return (
    <div className="app-chat" data-chat-pane data-collapsed={collapsed || undefined}>
      <button
        type="button"
        className="btn btn-ghost btn-sm app-chat-rail"
        aria-label={t("Expand assistant chat")}
        aria-expanded={!collapsed}
        onClick={() => onCollapsed(false)}
        data-waiting={waiting || undefined}
      >
        {waiting && <span className="app-chat-dot" role="status" aria-label={t("New reply waiting")} />}
        <span className="app-chat-rail-label text-micro text-ink-400">{t("ASSISTANT")}</span>
      </button>
      <div className="app-chat-head">
        <div className="app-chat-title min-w-0">
          <span className="app-chat-mark" aria-hidden>✳</span>
          <strong>{t("Assistant")}</strong>
          {(convLabel !== null || streamDown) && (
            <span
              className="app-chat-conv truncate text-ink-500 text-label"
              data-chat-conv-label
              data-state={streamDown ? "offline" : undefined}
            >
              · {streamDown ? t("Reconnecting…") : convLabel}
            </span>
          )}
        </div>
        <div className="app-chat-head-tools" role="toolbar" aria-label={t("Assistant tools")}>
          {active.state !== "legacy" && (
            <button
              type="button"
              className="app-chat-iconbtn"
              data-chat-new
              aria-label={t("New conversation")}
              title={t("New conversation")}
              disabled={!canCreate || creating}
              onClick={startNew}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M20 14.5a2 2 0 0 1-2 2H8l-4 3.5V6a2 2 0 0 1 2-2h12a2 2 0 0 1 2 2z" />
                <path d="M12 7.5v6M9 10.5h6" />
              </svg>
            </button>
          )}
          {active.state !== "legacy" && (
            <button
              ref={historyButtonRef}
              type="button"
              className="app-chat-iconbtn"
              aria-label={t("Conversations")}
              title={t("Conversations")}
              aria-haspopup="menu"
              aria-expanded={historyOpen}
              disabled={active.state !== "ready" || active.conversations.length === 0}
              onClick={() => setHistoryOpen((o) => !o)}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M3.5 12a8.5 8.5 0 1 0 2.6-6.1L3.5 8.5" />
                <path d="M3.5 3.5v5h5" />
                <path d="M12 7.5V12l3 2" />
              </svg>
              {hasUnreadOther && <span className="app-chat-iconbtn-dot" aria-hidden />}
            </button>
          )}
          <span className="app-chat-sep" aria-hidden />
          <button
            type="button"
            className="app-chat-iconbtn app-chat-collapse"
            aria-label={t("Collapse assistant chat")}
            title={t("Collapse assistant chat")}
            aria-expanded={!collapsed}
            onClick={() => onCollapsed(true)}
          >
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <rect x="3.5" y="4" width="17" height="16" rx="2.5" />
              <path d="M9.5 4v16" />
              <path d="m16 15-3-3 3-3" />
            </svg>
          </button>
        </div>
      </div>
      {historyOpen && active.state === "ready" && (
        <div
          ref={historyMenuRef}
          className="app-chat-history"
          role="menu"
          aria-label={t("Conversations")}
          data-chat-history
        >
          {active.conversations.map((c, i) => {
            const unread = isUnread(unreadSeen, c.id, c.updated) && c.id !== convId;
            return (
              <button
                key={c.id}
                type="button"
                role="menuitemradio"
                aria-checked={c.id === convId}
                className="app-chat-history-row"
                data-unread={unread || undefined}
                onClick={() => {
                  selectConversation(installId, c.id);
                  closeHistory();
                }}
              >
                <span className="app-chat-history-mark" aria-hidden>
                  {c.id === convId ? "✓" : ""}
                </span>
                <span className="app-chat-history-name">{conversationLabel(c, i, subjects, t)}</span>
                {unread && (
                  <span className="app-chat-history-new" aria-label={t("New reply")}>
                    {t("new")}
                  </span>
                )}
              </button>
            );
          })}
          {active.selected !== null &&
            !active.selected.isGeneral &&
            binding.scope !== null &&
            binding.error === null &&
            binding.scope.install_id === installId &&
            binding.scope.context_id !== "" &&
            mode.contextProof?.status === "ready" &&
            mode.contextProof.contextId === binding.scope.context_id &&
            mode.appPath != null && (
              <>
                <div className="app-chat-history-divider" role="separator" />
                <Link
                  href={`${mode.appPath}?install=${encodeURIComponent(installId)}&ctx=${encodeURIComponent(binding.scope.context_id)}&conversation=${encodeURIComponent(active.selected.id)}`}
                  className="app-chat-history-row"
                  role="menuitem"
                  aria-label={t("Link to this conversation")}
                  title={t("Shareable link to this conversation")}
                  data-chat-conversation-link
                >
                  <span className="app-chat-history-mark" aria-hidden />
                  <span className="app-chat-history-name">{t("Link to this conversation")}</span>
                </Link>
              </>
            )}
        </div>
      )}
      {active.requestPending && (
        <p className="text-label text-ink-500" role="status" data-chat-link-pending>
          {t("Checking the linked conversation and its context…")}
        </p>
      )}
      {active.requestError !== null && (
        <p className="text-label text-fail" role="alert" data-chat-link-error>
          {t(active.requestError)}
        </p>
      )}
      {active.state === "failed" && (
        <p className="text-label text-fail" role="alert">
          {t("The conversations could not be read")} — {active.error}{" "}
          <button type="button" className="lnk" onClick={active.retry}>
            {t("Retry")}
          </button>
        </p>
      )}
      {active.state === "ready" && convId === null && (
        <p className="text-label text-ink-500" data-empty="conversations">
          {t("No conversation yet. Start one with + New.")}
        </p>
      )}
      <TurnBar state={turnState} since={masterState.data?.turn?.since ?? null} actions={turnActions} reconnecting={streamDown} />
      {thread.status === "failed" && (
        <p className="text-label text-fail" role="alert">
          {t("The thread could not be read")} — {thread.error}{" "}
          <button type="button" className="lnk" onClick={() => void store.refresh()}>
            {t("Retry")}
          </button>
        </p>
      )}
      {usable && !loaded && thread.status !== "failed" && (
        <p className="text-label text-ink-500" role="status">
          {t("Reading the thread…")}
        </p>
      )}
      {usable && loaded && items.length === 0 && (
        <p className="text-label text-ink-500" data-empty="chat">
          {t("Nothing here yet. Send the first message.")}
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
            {loadingEarlier ? t("Loading…") : t("Load earlier messages ↑")}
          </button>
        </div>
      )}
      <ol className="app-chat-list" ref={listRef} onScroll={onListScroll} role="log" aria-live="polite" aria-relevant="additions" aria-label={t("Conversation messages")}>
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
              question={visibleQuestion(shown, i)}
            />
          </li>
        ))}
      </ol>
      <AssistantOperations key={`${installId}\u0000${mode.contextId}`} installId={installId} contextId={mode.contextId} appPath={mode.appPath ?? null} canDecide={viewer.operator === true && !viewer.readOnly} onPendingChange={setOperationPending} />
      {unseen > 0 && (
        <button type="button" className="lnk text-label app-chat-jump" onClick={jumpToLatest} data-jump-to-latest>
          ↓ {jumpLabel(unseen, t)}
        </button>
      )}
      {sendError && (
        <p className="text-label text-fail" role="alert">
          {t(sendError)}
        </p>
      )}
      {binding.error !== null && binding.error !== sendError && (
        <p className="text-label text-fail" role="alert" data-chat-binding-error>
          {t(binding.error)}
        </p>
      )}
      {composerNotice && (
        <p className="text-micro text-ink-400" role="status" data-chat-notice>
          {displayUiNotice(composerNotice, t)}
        </p>
      )}
      {failedOutbox !== null && (
        <div className="app-chat-outbox" data-chat-outbox ref={outboxRef}>
          <p className="text-micro text-ink-400">{t("Not sent — the conversation could not be created.")}</p>
          <p className="text-secondary text-ink-300 break-words">{failedOutbox.text}</p>
          {failedOutbox.error && <p className="text-micro text-ink-500 break-words">{failedOutbox.error}</p>}
          <p className="text-micro">
            <button type="button" className="lnk" onClick={() => retryOutbox(failedOutbox)}>
              {t("Retry")}
            </button>{" "}
            ·{" "}
            <button type="button" className="lnk" onClick={() => discardOutbox(failedOutbox)}>
              {t("Discard")}
            </button>
          </p>
        </div>
      )}
      {savedIntents.length > 0 && (
        <div className="app-chat-outbox" data-chat-saved-intent>
          <p className="text-micro text-ink-400">
            {savedIntents.length === 1
              ? t("An unsent draft is kept — this conversation already had its own.")
              : `${formatNumber(savedIntents.length)} ${t("unsent drafts are kept — this conversation already had its own.")}`}
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
                  {savedIntents.length === 1 ? t("Restore saved draft") : `${t("Restore saved draft")} ${formatNumber(i + 1)}`}
                </button>
              </span>
            ))}
          </p>
        </div>
      )}
      {/* CAD-1431: the CSV import is an explicit composer action, not a
          standing panel. It stays scoped to this install and context (the
          key remounts it on a change) and is independent of the generic
          file-upload grant. */}
      {csvAvailable && csvMounted && (
        <div className="app-chat-import" data-chat-attachment="csv-import" hidden={!csvOpen}>
          <div className="app-chat-import-head">
            <span className="text-label text-ink-300">{t("Import customer CSV…")}</span>
            <button type="button" className="lnk text-label" onClick={() => setCsvOpen(false)}>
              {t("Close")}
            </button>
          </div>
          {renderCapability("csv-import", {
            scope: { installId: binding.scope!.install_id, contextId: binding.scope!.context_id },
            canWrite: canSend,
            sendIntent: async (intent) => {
              const message = newMessageId();
              try {
                await withPrivateOperation(async (op) => {
                  await api.threadSend(
                    MASTER,
                    JSON.stringify(intent),
                    message,
                    undefined,
                    sendApp,
                    convId ?? undefined,
                    undefined,
                    op,
                  );
                  op.assertCurrent();
                  void store.refresh();
                  void resources.masterState.refresh();
                });
                return null;
              } catch (e: unknown) {
                return e instanceof ApiError ? e.message : String(e);
              }
            },
          })}
        </div>
      )}
      {/* CAD-1168: the shared composer at compact density. The key
          remounts it per conversation, so each conversation's draft and
          its citation refs (persisted with the same key) are restored and
          no other's leaks in. CAD-1174 keeps the context concept off the
          surface: no default chip, and a record chip waits for a
          server-proven reference rather than the route's `recordOpen`
          flag. CAD-1431: the starters show only in a loaded, empty
          conversation with a clear composer; a click fills and focuses it. */}
      {ctx !== null && ctx.prompts.length > 0 && starters && (
        <div className="app-chat-sugg" data-chat-context data-chat-prompts>
          {ctx.prompts.map((p) => (
            <button
              key={p}
              type="button"
              className="app-chat-sugg-row"
              onClick={() => {
                // Fill and focus only. Never sends, and never replaces an
                // intent that appeared since the list was painted.
                if (composerHasIntent(composerKey)) return;
                setComposerDraft(composerKey, p);
                document.getElementById(COMPOSER_ID)?.focus();
              }}
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
        onSend={({ body, refs, attachments, mediaRefs }) => send(body, refs, attachments, mediaRefs)}
        onSubmitted={engage}
        onCommand={(name) => {
          if (name === "new" || name === "clear") startNew();
          else setComposerNotice({ key: 'Unsupported command "/{name}" — it was not sent', slots: { name } });
        }}
        onNotice={setComposerNotice}
        attach={{ enabled: canAttach, reason: attachReason, destination: attachDestination }}
        originalUnresolved={originalUnresolved !== null}
        onResolveOriginal={() => outboxRef.current?.scrollIntoView?.({ block: "nearest" })}
        textareaId={COMPOSER_ID}
        actions={csvActions}
        ariaLabel={t("Message to Assistant")}
        placeholder={t("Message Assistant…")}
        className="app-chat-form"
      />
    </div>
  );
}
