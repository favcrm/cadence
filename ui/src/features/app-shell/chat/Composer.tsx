import { askBody, askLabel, citesArtifact } from "../../canvas/askRefs";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  useSyncExternalStore,
  type KeyboardEvent,
  type MouseEvent,
} from "react";
import type { ThreadRef } from "../../../lib/types";
import { useLocale } from "../../../lib/locale";
import { parseSlash, SLASH_COMMANDS, slashMatches, WORKSPACE_SKILLS_COMMAND, type SlashCommand } from "../../home/master";
import type { PendingAttachment } from "../../home/thread";
import WorkspaceSkillPicker from "../../home/WorkspaceSkillPicker";
import MentionPicker from "../../mentions/MentionPicker";
import {
  applyMention,
  filterMentionCandidates,
  findActiveMention,
  startsWithNote,
  type MentionCandidate,
} from "../../mentions/mentions";
import { attachMeta, displayAttachError, unresolvedHint, unresolvedHintNotice, type AttachDestination, type AttachItem } from "./attach";
import type { UiNotice } from "../../../lib/uiNotice";
import {
  addFiles,
  attachHold,
  attachItems as storedAttachItems,
  captureReady,
  composerDraftState,
  consumeSeed,
  onComposerFocusRequest,
  removeAttach,
  reserveDraft,
  retryAttach,
  setComposerContent,
  setComposerDraft,
  setComposerRefs,
  settleDraft,
  settleReady,
  subscribeAttach,
  subscribeDraft,
  removeLibraryImagePin,
} from "./composerStore";

/**
 * The ONE composer (CAD-1168): Home and every app pane render the same
 * input stack. It lives beside the other shared chat pieces
 * (Conversation, directives) rather than under `home/` because
 * `app-shell/chat` is the layer both surfaces already converge on —
 * Home keeps only the full-density extras passed in as props.
 *
 * The draft, its auto-grow and the IME-safe Enter/Shift+Enter behaviour
 * are owned here. Draft and attachment-queue lifetime are the caller's:
 * both are held in `composerStore` under a caller-chosen key, so a
 * remount (conversation switch, Home route change) restores them and an
 * in-flight upload keeps running while the composer is unmounted.
 *
 * Density changes layout, never access: both densities share the same
 * Attach picker/drop/chips and the same send-gating rules. `compact`
 * (app panes, the approved CAD-1168 mock) renders the bordered card,
 * icon Attach and the circular up-arrow Send; `full` (Home) keeps its
 * slash menu and ref chips beside the same controls.
 *
 * Send is withheld while any queued file is unresolved — uploading or
 * failed — so an operator's Enter can never silently drop a source
 * (the defect this slice repairs). A resolved send clears only the
 * rows it actually carried; failed rows stay for Retry/remove. An
 * upload that completes after its row was removed updates nothing.
 */
export interface ComposerSend {
  body: string;
  refs: ThreadRef[];
  /** CAD-1168: daemon-minted ids of the retained uploads that are `ready`
   *  (plus local metadata so the pending bubble renders chips/previews). */
  attachments?: PendingAttachment[];
  /** One host-resolved library image pin. Not a file upload. */
  mediaRefs?: { asset_id: string; revision: number; sha256: string }[];
}

export type { AttachItem };

export interface ComposerProps {
  density: "compact" | "full";
  /** Why sending is blocked (read-only, absent master), or null. */
  block: string | null;
  /**
   * The draft/queue key — `home`, or `install|conversation`. The store
   * is module-scoped and keyed, so a remount restores this scope's text,
   * citation refs and files and never another's; an in-flight upload
   * survives the unmount. Required when the composer may unmount.
   */
  storeKey?: string;
  /** Replaces the draft; `n` is a reseed counter (a repeated seed still applies). */
  seed?: { text: string; n: number; refs?: ThreadRef[] };
  /**
   * Full density only: a complete `/verb [arg]` submits through here
   * (never to the thread). Compact panes with their own commands
   * (`/new`) pass it too; without it every submission is a send. `false`
   * means the words did not run and may be a message, so the box keeps them.
   */
  onCommand?: (name: string, arg: string) => boolean | void;
  /** The verbs the menu offers, when this board cannot run them all. */
  commands?: readonly SlashCommand[];
  /** A real send completed validation — deliver `body` (with `refs`).
   *  The returned promise settles when the destination's pending row
   *  owns the envelope; a rejected promise keeps the draft and files. */
  onSend: (send: ComposerSend) => void | Promise<void>;
  /** Any submission (send or command) — pin/follow hooks, before the send paints. */
  onSubmitted?: () => void;
  /** Focus the box after a send (the Home dock does). */
  refocusAfterSend?: boolean;
  /** Reveal an owning pane before an external request focuses this composer. */
  onReveal?: () => void;
  textareaId?: string;
  ariaLabel?: string;
  placeholder?: string;
  blockedPlaceholder?: string;
  className?: string;
  /** Extra form attributes — Home's `data-composer` hook is one. */
  formProps?: Record<string, string | undefined>;
  /** The shared Attach picker/drop/chips. App mode enables it only from
   *  the live native projection; Home binds to its native Master thread. */
  attach?: boolean | {
    enabled: boolean;
    reason: string;
    destination?: AttachDestination;
  };
  /** A submission that is neither a send nor a known command surfaces
   *  this one-line notice (the unsupported-slash refusal and the
   *  unresolved-attachment hold). */
  onNotice?: (text: UiNotice) => void;
  /** The original submitted envelope is unresolved (a failed create kept
   *  it in the pane's outbox): an ordinary new submission is refused
   *  until the operator retries or discards it. Typing, refs and files
   *  stay editable — the box is never disabled and nothing is silently
   *  discarded. */
  originalUnresolved?: boolean;
  /** The operator asked to settle the unresolved original: the pane
   *  reveals its recovery control. */
  onResolveOriginal?: () => void;
  /** Compact only: host actions (CAD-1431) listed in a More actions menu. */
  actions?: ComposerAction[];
  /**
   * CAD-1880: this pane's turn is stoppable. Stop takes the send slot while
   * a turn runs — the dock no longer carries a working row, so the action
   * that interrupts the turn sits where the next message would be sent.
   * Absent while the stream is down (last known state) and in read-only.
   */
  stop?: { onStop: () => void } | null;
  /** Optional Up-arrow handler when the composer is empty and no menu is open. */
  onArrowUpEmpty?: () => boolean;
  /** Optional `@mention` candidates shown when typing `@...`. */
  mentionCandidates?: MentionCandidate[];
  /** Allow leading `/note` without invoking the slash-command dispatcher. */
  allowNote?: boolean;
  /** Observe draft text changes (e.g. for private Home mention previews). */
  onDraftChange?: (draft: string) => void;
  /** Optional pre-send gate; returning `false` holds the draft in place. */
  beforeSend?: (body: string) => boolean;
  /** Optional `/note` silent-post toggle (Channels). */
  showSilentToggle?: boolean;
  silent?: boolean;
  onSilentChange?: (next: boolean) => void;
}

/** One host action behind the compact composer's More actions menu. */
export interface ComposerAction {
  label: string;
  onSelect: () => void;
}

/** CAD-2422: a message longer than this (UTF-8) is sent as a text attachment.
 *  Large enough for any ordinary message; small next to the 256 KiB model request. */
const LONG_TEXT_BYTES = 32 * 1024;
/** CAD-2422: per composer key, the long text already copied into an
 *  attachment row and that row, so the next send carries the row in its
 *  place while the text itself never leaves the box until it is sent. */
const pastedText = new Map<string, { text: string; itemKey: string }>();

/** Draft attachment chips (CAD-1419): one icon slot per chip that
 *  becomes Remove on hover/focus and stays visible on touch. Up to three
 *  show; the rest sit behind +N more, with any failed or uploading file
 *  surfaced first so an unresolved file is never hidden silently. */
const CHIP_LIMIT = 3;

/** Where keyboard focus goes after a keyboard-initiated removal: the row
 *  that took the removed row's place, else the one before it. */
interface RemoveFocusTarget {
  removed: string;
  next: string | null;
  prev: string | null;
}

function AttachChips({
  items,
  t,
  onRetry,
  onRemove,
}: {
  items: AttachItem[];
  /** The shared attach translator (`translateAttachText`), the same one
   *  `attachMeta` and `displayAttachError` receive everywhere else. */
  t: (english: string) => string;
  onRetry: (item: AttachItem) => void;
  onRemove: (item: AttachItem, focus: RemoveFocusTarget | null) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  // Failed first, then uploading, then ready: the rows behind +N more
  // are the ones that still need the operator.
  const rank = (a: AttachItem) => (a.status === "failed" ? 0 : a.status === "uploading" ? 1 : 2);
  const ordered = [...items].sort((a, b) => rank(a) - rank(b));
  const visible = expanded || ordered.length <= CHIP_LIMIT ? ordered : ordered.slice(0, CHIP_LIMIT);
  const hidden = ordered.slice(visible.length);
  // Unresolved rows behind +N more must stay discoverable: the button says
  // so, and the failure is named on it.
  const hiddenFailed = hidden.some((a) => a.status === "failed");
  const hiddenPending = hidden.some((a) => a.status === "uploading");
  return (
    <div className="app-chat-attachrow" data-composer-attachments>
      {visible.map((a, i) => (
        <div
          key={a.key}
          role="group"
          className={`app-chat-filerow${a.status === "failed" ? " failed" : ""}`}
          data-attach-status={a.status}
          data-attach-key={a.key}
          aria-label={`${a.name}: ${t(a.status === "uploading" ? "Uploading…" : a.status === "failed" ? "Failed" : "Ready")}${a.error ? ` — ${displayAttachError(a.error, t)}` : ""}`}
          title={a.name}
        >
          <span className="app-chat-chip-slot">
            <span className="app-chat-chip-status" aria-hidden="true">
              {a.status === "uploading" ? (
                <span className="app-chat-spinner" />
              ) : a.status === "failed" ? (
                <svg viewBox="0 0 24 24"><path d="M12 4l9 16H3zM12 10v4M12 17h.01" /></svg>
              ) : (
                <svg viewBox="0 0 24 24"><path d="M5 3h9l5 5v13H5zM14 3v6h5" /></svg>
              )}
            </span>
            {/* A failed row keeps its warning glyph and puts Remove at the
                trailing edge, so the way out is visible without hovering. */}
            {a.status !== "failed" && (
              <button
                type="button"
                className="app-chat-remove"
                aria-label={`${t("Remove")} ${a.name}`}
                title={`${t("Remove")} ${a.name}`}
                onClick={(e: MouseEvent<HTMLButtonElement>) => {
                  // Only keyboard or assistive activation (detail 0) on the
                  // button that holds focus moves focus; a pointer click never does.
                  const keyboard = e.detail === 0 && document.activeElement === e.currentTarget;
                  onRemove(a, keyboard
                    ? { removed: a.key, next: ordered[i + 1]?.key ?? null, prev: ordered[i - 1]?.key ?? null }
                    : null);
                }}
              >
                <svg viewBox="0 0 24 24" aria-hidden="true">
                  <path d="M6 6l12 12M18 6L6 18" />
                </svg>
              </button>
            )}
          </span>
          {/* One column holds the name/meta line and, when the file failed,
              its reason: both share the left edge right of the icon slot. */}
          <span className="app-chat-filebody">
            <span className="app-chat-filename">
              <span className="app-chat-chip-name">{a.name}</span>
              <small>
                {attachMeta(a.name, a.size, t)}
                {a.status !== "ready" && (
                  <span className="app-chat-touch-status">{` · ${t(a.status === "uploading" ? "Uploading…" : "Failed")}`}</span>
                )}
              </small>
            </span>
            {a.status === "failed" && a.error && (
              <small className="app-chat-upload-error" role="alert" title={displayAttachError(a.error, t)}>
                {displayAttachError(a.error, t)}
              </small>
            )}
          </span>
          {a.status === "failed" && !a.permanent && (
            <button type="button" className="lnk app-chat-chip-retry" onClick={() => onRetry(a)}>
              {t("Retry")}
            </button>
          )}
          {a.status === "failed" && (
            <button
              type="button"
              className="app-chat-remove app-chat-remove-trailing"
              aria-label={`${t("Remove")} ${a.name}`}
              title={`${t("Remove")} ${a.name}`}
              onClick={(e: MouseEvent<HTMLButtonElement>) => {
                const keyboard = e.detail === 0 && document.activeElement === e.currentTarget;
                onRemove(a, keyboard
                  ? { removed: a.key, next: ordered[i + 1]?.key ?? null, prev: ordered[i - 1]?.key ?? null }
                  : null);
              }}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M6 6l12 12M18 6L6 18" />
              </svg>
            </button>
          )}
        </div>
      ))}
      {hidden.length > 0 && (
        <button
          type="button"
          className={`app-chat-more lnk${hiddenFailed ? " failed" : ""}`}
          aria-expanded={expanded}
          onClick={() => setExpanded(true)}
        >
          +{hidden.length} {t("more")}
          {hiddenFailed ? ` · ${t("failed")}` : hiddenPending ? ` · ${t("Uploading…")}` : ""}
        </button>
      )}
      {expanded && ordered.length > CHIP_LIMIT && (
        <button type="button" className="app-chat-more lnk" aria-expanded={expanded} onClick={() => setExpanded(false)}>
          {t("Show less")}
        </button>
      )}
    </div>
  );
}

/** The More actions menu: an on-demand list of host actions, closed by a
 *  pick, Escape or a pointer press outside it. */
function ComposerActions({ actions, disabled }: { actions: ComposerAction[]; disabled: boolean }) {
  const { t } = useLocale();
  const [open, setOpen] = useState(false);
  const wrap = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: Event) => {
      if (!wrap.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", close);
    return () => document.removeEventListener("pointerdown", close);
  }, [open]);
  return (
    <div className="app-chat-actions" ref={wrap} onKeyDown={(e) => { if (e.key === "Escape") setOpen(false); }}>
      <button
        type="button"
        className="app-chat-attach"
        aria-label={t("More actions")}
        aria-expanded={open}
        title={t("More actions")}
        disabled={disabled}
        onClick={() => setOpen((o) => !o)}
      >
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="M5 12h.01M12 12h.01M19 12h.01" />
        </svg>
      </button>
      {open && (
        <div className="app-chat-actions-menu" aria-label={t("More actions")}>
          {actions.map((a) => (
            <button
              key={a.label}
              type="button"
              className="app-chat-actions-item"
              onClick={() => {
                setOpen(false);
                a.onSelect();
              }}
            >
              {a.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

/** The shared icon Send: one renderer for both densities (CAD-1168).
 *  Circular, monochrome, accessible; disabled while the send is held. */
function IconSend({ disabled, title }: { disabled: boolean; title: string }) {
  const { t } = useLocale();
  return (
    <button type="submit" className="app-chat-send" aria-label={t("Send message")} title={t(title)} disabled={disabled}>
      {/* Visually-hidden label keeps textContent "Send" for the tests
          and any CSS-free reader. */}
      <span className="sr-only">{t("Send")}</span>
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="M12 19V5" />
        <path d="M5 12l7-7 7 7" />
      </svg>
    </button>
  );
}

/** The running turn's Stop (CAD-1880): same slot and footprint as Send,
 *  danger-tinted so the interruption never reads as the primary action. */
function IconStop({ onStop }: { onStop: () => void }) {
  const { t } = useLocale();
  return (
    <button
      type="button"
      className="app-chat-send app-chat-stop"
      aria-label={t("Stop this turn")}
      title={t("Stop")}
      onClick={onStop}
    >
      <span className="sr-only">{t("Stop")}</span>
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <rect x="6" y="6" width="12" height="12" rx="2" />
      </svg>
    </button>
  );
}

export default function Composer({
  density,
  block,
  storeKey,
  seed,
  onCommand,
  commands,
  onSend,
  onSubmitted,
  refocusAfterSend,
  onReveal,
  textareaId,
  ariaLabel,
  placeholder,
  blockedPlaceholder = "Read-only · Sending is unavailable",
  className,
  formProps,
  attach = false,
  onNotice,
  originalUnresolved = false,
  onResolveOriginal,
  actions,
  stop = null,
  onArrowUpEmpty,
  mentionCandidates,
  allowNote = false,
  onDraftChange,
  beforeSend,
  showSilentToggle = false,
  silent = false,
  onSilentChange,
}: ComposerProps) {
  const { t } = useLocale();
  const compact = density === "compact";
  const translateAttachText = t;
  const key = storeKey ?? "composer";
  // The draft cell is the ONE owner of text, refs, edit version and the
  // in-flight handoff reservation. The box renders this snapshot, so a
  // remount and a settlement can never leave a stale second copy.
  const subscribeDraftCell = useCallback((listener: () => void) => subscribeDraft(key, listener), [key]);
  const readDraftCell = useCallback(() => composerDraftState(key), [key]);
  const cell = useSyncExternalStore(subscribeDraftCell, readDraftCell);
  const draft = cell.text;
  const refs = cell.refs;
  const hasArtifactRef = citesArtifact(refs);
  const libraryImage = cell.libraryImage;
  const libraryPin = libraryImage?.state === "ready" ? libraryImage.pin : null;
  const onDraftChangeRef = useRef(onDraftChange);
  onDraftChangeRef.current = onDraftChange;
  useEffect(() => {
    onDraftChangeRef.current?.(draft);
  }, [draft]);
  const setDraft = (next: string) => setComposerDraft(key, next);
  const setRefs = (next: ThreadRef[]) => setComposerRefs(key, next);
  // The queue lives in the store, keyed like the draft: a remount
  // restores it and an in-flight upload keeps running. `useSyncExternalStore`
  // re-renders this composer on every queue change.
  const subscribeQueue = useCallback((listener: () => void) => subscribeAttach(key, listener), [key]);
  const readQueue = useCallback(() => storedAttachItems(key), [key]);
  const attachItems = useSyncExternalStore(subscribeQueue, readQueue);
  const [dropping, setDropping] = useState(false);
  const fileInput = useRef<HTMLInputElement>(null);
  // A keyboard removal waits here until its commit paints, so focus lands
  // on the surviving row's own Remove button (or the composer when none).
  const pendingFocus = useRef<RemoveFocusTarget | null>(null);
  useEffect(() => {
    const pending = pendingFocus.current;
    if (!pending) return;
    pendingFocus.current = null;
    // A no-op removal (the row was already gone) moves nothing.
    if (attachItems.some((a) => a.key === pending.removed)) return;
    const form = box.current?.form;
    const rowRemove = (k: string | null) => {
      if (k === null || !form) return null;
      const row = Array.from(form.querySelectorAll<HTMLElement>("[data-attach-key]")).find((el) => el.dataset.attachKey === k);
      return row?.querySelector<HTMLButtonElement>(".app-chat-remove") ?? null;
    };
    (rowRemove(pending.next) ?? rowRemove(pending.prev) ?? box.current)?.focus({ preventScroll: true });
  });
  const [hi, setHi] = useState(0);
  // Esc dismisses the menu for THIS verb text — the next keystroke
  // reopens it (or a cleared draft does).
  const [menuOffFor, setMenuOffFor] = useState<string | null>(null);
  const [skillsFor, setSkillsFor] = useState<string | null>(null);
  const [caret, setCaret] = useState<number>(0);
  const [mentionHi, setMentionHi] = useState<number>(0);
  const [mentionOffFor, setMentionOffFor] = useState<string | null>(null);
  const box = useRef<HTMLTextAreaElement>(null);
  const revealRef = useRef(onReveal);
  revealRef.current = onReveal;
  useLayoutEffect(() => {
    let frame: number | null = null;
    const off = onComposerFocusRequest((requestedKey) => {
      if (requestedKey !== key) return;
      revealRef.current?.();
      if (frame !== null) cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        frame = null;
        box.current?.focus();
      });
    });
    return () => {
      off();
      if (frame !== null) cancelAnimationFrame(frame);
    };
  }, [key]);
  useEffect(() => {
    if (seed && seed.n > 0 && consumeSeed(key, seed)) {
      setComposerContent(key, seed.text, seed.refs ?? []);
      if (!block) box.current?.focus();
    }
  }, [seed]);
  // The box can mount while its pane is hidden. Size it once visible and
  // whenever its available width changes, as well as when its text changes.
  const resizeBox = useCallback(() => {
    const el = box.current;
    if (!el || el.clientWidth === 0) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, []);
  useEffect(() => {
    resizeBox();
  }, [draft, block, blockedPlaceholder, placeholder, compact, t, hasArtifactRef, resizeBox]);
  useEffect(() => {
    const el = box.current;
    // No observer where the API is missing; the initial measure above stays.
    if (!el || typeof ResizeObserver === "undefined") return;
    let width = el.clientWidth;
    const observer = new ResizeObserver(() => {
      if (el.clientWidth === width) return;
      width = el.clientWidth;
      resizeBox();
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, [compact, resizeBox]);

  // Slash completion is a full-density extra (Home's commands); compact
  // mode routes slash verbs through onCommand without sending them
  // as ordinary chat.
  const skillsEnabled = !compact && commands?.includes(WORKSPACE_SKILLS_COMMAND);
  const browse = skillsEnabled ? /^\/skills(?:\n([\s\S]*))?$/i.exec(draft.trim()) : null;
  const skillPickerOpen = !block && browse !== null && draft === skillsFor;
  const menu = !compact && (onCommand !== undefined || skillsEnabled);
  const slash = menu ? parseSlash(draft) : null;
  const verbDraft = menu && draft.startsWith("/") && !draft.includes(" ") ? draft.slice(1) : null;
  const matches =
    !skillPickerOpen && verbDraft !== null && draft !== menuOffFor ? slashMatches(verbDraft.toLowerCase(), commands) : [];
  useEffect(() => setHi(0), [verbDraft]);
  const hiClamped = Math.min(hi, Math.max(0, matches.length - 1));

  const complete = (c: SlashCommand) => {
    setDraft(`/${c.name}${c.arg ? " " : ""}`);
    setMenuOffFor(null);
  };

  const activeMention =
    !compact &&
    !block &&
    !skillPickerOpen &&
    matches.length === 0 &&
    mentionCandidates &&
    mentionCandidates.length > 0
      ? findActiveMention(draft, caret)
      : null;
  const mentionSpanKey = activeMention ? `${activeMention.start}:${activeMention.query}` : null;
  const mentionMatches =
    activeMention && mentionCandidates && mentionSpanKey !== mentionOffFor
      ? filterMentionCandidates(mentionCandidates, activeMention.query)
      : [];
  const selectableMentionIndices = mentionMatches
    .map((candidate, index) => (candidate.active ? index : -1))
    .filter((index) => index >= 0);
  const mentionHiClamped =
    selectableMentionIndices.length === 0
      ? -1
      : selectableMentionIndices.includes(mentionHi)
        ? mentionHi
        : selectableMentionIndices[0]!;
  useEffect(() => {
    setMentionHi(0);
  }, [mentionSpanKey]);

  const completeMention = (candidate: MentionCandidate) => {
    if (!activeMention || !candidate.active) return;
    const applied = applyMention(draft, activeMention, candidate);
    setDraft(applied.text);
    setCaret(applied.caret);
    setMentionOffFor(null);
    requestAnimationFrame(() => {
      const el = box.current;
      if (!el) return;
      el.focus();
      el.setSelectionRange(applied.caret, applied.caret);
    });
  };

  // A command has gone: empty the box, which closes the menu, and keep focus.
  const finishCommand = () => {
    setDraft("");
    setMenuOffFor(null);
    if (refocusAfterSend) box.current?.focus();
  };

  // A bare refused verb is a typo, so the next keystroke replaces it instead of
  // joining it (`/nosuch/new`). Words after a verb are the person's to edit.
  const keepRefused = (arg: string) => {
    if (arg === "") box.current?.select();
  };

  const submit = () => {
    const body = draft.trim();
    // `block` is also "not an operator" in an app pane — the box and
    // Send are disabled and no submission leaves the composer.
    if (!body || block) return;
    if (browse) {
      setSkillsFor(draft);
      setMenuOffFor(draft);
      return;
    }
    // An unresolved original submission owns this key: a new ordinary
    // submission is refused until the operator retries or discards it,
    // so the earlier intent can never be silently replaced. The newer
    // text, refs and files stay editable and are not discarded.
    if (originalUnresolved) {
      onNotice?.("The previous message was not sent — retry or discard it before sending again");
      onResolveOriginal?.();
      return;
    }
    if (libraryImage?.state === "pending") {
      onNotice?.("Wait for the image reference to attach, or remove it before sending");
      return;
    }
    // A queued file that is still uploading or that failed is never
    // silently dropped: the send waits until every row is ready or
    // removed, and the notice says exactly why.
    const hold = unresolvedHintNotice(attachHold(key));
    if (hold !== null) {
      onNotice?.(hold);
      return;
    }
    // CAD-2422: a message is never compacted or shortened on its way to the
    // model, so a very long one goes as a text file the assistant pages
    // through instead (`chat_attachment_read`). The first send copies the
    // text into an attachment row and stops; the text stays in the box, so a
    // failed or removed upload never loses it. Once that row is ready, the
    // next send carries the row with a short line instead of the text.
    let outgoing = body;
    const bodyBytes = new TextEncoder().encode(body).length;
    if (bodyBytes > LONG_TEXT_BYTES && !parseSlash(body)) {
      const kb = String(Math.ceil(bodyBytes / 1024));
      if (!attachEnabled) {
        onNotice?.({ key: "This message is too long to send ({kb} KB, the limit is 32 KB). Attach it as a text file instead.", slots: { kb } });
        return;
      }
      const pasted = pastedText.get(key);
      const row = pasted && storedAttachItems(key).some((item) => item.key === pasted.itemKey);
      if (pasted && row && pasted.text === body) {
        // The hold above already stopped a send while the row uploads or
        // failed, so the row is ready here.
        outgoing = t("Please read the attached text.");
      } else {
        // An edited text replaces its earlier copy rather than adding one.
        if (pasted && row) removeAttach(key, pasted.itemKey);
        const name = `pasted-text-${new Date().toISOString().replace(/[:.]/g, "-")}.txt`;
        addFiles(key, [new File([body], name, { type: "text/plain" })], attachDestination);
        const added = storedAttachItems(key).at(-1);
        if (added) pastedText.set(key, { text: body, itemKey: added.key });
        onNotice?.({ key: "The long text ({kb} KB) will be sent as {name}. Press Send again once it has uploaded; the text stays here until then.", slots: { kb, name } });
        return;
      }
    }
    const cmd = (showSilentToggle || allowNote) && startsWithNote(body) ? null : parseSlash(body);
    if (cmd && onCommand === undefined) {
      onSubmitted?.();
      // A verb that the host does not handle is not ordinary chat.
      onNotice?.({ key: 'Unsupported command "/{name}" — it was not sent', slots: { name: cmd.name } });
      return;
    } else if (cmd) {
      onSubmitted?.();
      if (onCommand?.(cmd.name, cmd.arg) !== false) finishCommand();
      else keepRefused(cmd.arg);
    } else {
      if (beforeSend && beforeSend(outgoing) === false) return;
      // Pin before a pending row paints, so a follow effect scrolls into view.
      onSubmitted?.();
      // Capture the ready rows by key+generation, not by file id: two
      // distinct rows can legitimately share a dedup id, and a later row
      // must survive this send's settlement.
      let ready: ReturnType<typeof captureReady>;
      try {
        ready = captureReady(key, attachEnabled ? attachDestination : undefined);
      } catch (e) {
        onNotice?.(e instanceof Error ? e.message : String(e));
        return;
      }
      // Reserve this key's original envelope before handing it off: a
      // second submit (double event, or a remount whose box still shows
      // the text) finds the reservation and is refused. The reservation
      // lives in the store, so it survives the unmount — and its handle
      // still finds the cell if the scope moves to the created
      // conversation while the handoff is in flight.
      const reservation = reserveDraft(key);
      if (reservation === null) {
        onNotice?.("Still sending the previous message — wait for it to settle");
        return;
      }
      // The whole envelope — original key, body, refs, ready files — is
      // handed to the destination's pending row before anything is
      // cleared. A rejected handoff releases the reservation and keeps
      // the draft, refs and files for an explicit retry.
      let handoff: void | Promise<void>;
      try {
        handoff = onSend({
          // A cited artifact is expanded into the body: refs are stored for the
          // board, only the body reaches the agent.
          body: askBody(outgoing, refs),
          refs,
          // Only ready rows ride a send — gating above already withheld
          // it while anything unresolved remained.
          attachments: ready.ids,
          mediaRefs: libraryPin
            ? [{ asset_id: libraryPin.asset_id, revision: libraryPin.revision, sha256: libraryPin.sha256 }]
            : undefined,
        });
      } catch {
        // A synchronous handoff failure must release the reservation,
        // not wedge the composer behind a handoff that never settled.
        settleDraft(reservation, false);
        return;
      }
      const settle = (accepted: boolean) => {
        if (accepted && outgoing !== body) pastedText.delete(key);
        if (accepted) {
          // Remove exactly the rows this send carried; a failed row, a
          // file queued after the send's resolution, or a later distinct
          // row that shares the retained id stays put.
          settleReady(ready);
        }
        settleDraft(reservation, accepted);
        if (accepted && refocusAfterSend) box.current?.focus();
      };
      if (handoff && typeof (handoff as Promise<void>).then === "function") {
        void (handoff as Promise<void>).then(() => settle(true), () => settle(false));
      } else {
        settle(true);
      }
    }
  };
  const onKey = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.nativeEvent.isComposing || e.nativeEvent.keyCode === 229) return;
    if (mentionMatches.length > 0 && activeMention) {
      if (e.key === "ArrowDown" && selectableMentionIndices.length > 0) {
        e.preventDefault();
        const curPos = selectableMentionIndices.indexOf(mentionHiClamped);
        const nextPos = curPos < 0 ? 0 : Math.min(curPos + 1, selectableMentionIndices.length - 1);
        setMentionHi(selectableMentionIndices[nextPos]!);
        return;
      }
      if (e.key === "ArrowUp" && selectableMentionIndices.length > 0) {
        e.preventDefault();
        const curPos = selectableMentionIndices.indexOf(mentionHiClamped);
        const prevPos = curPos <= 0 ? 0 : curPos - 1;
        setMentionHi(selectableMentionIndices[prevPos]!);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        setMentionOffFor(mentionSpanKey);
        return;
      }
      if (
        mentionHiClamped >= 0 &&
        (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey))
      ) {
        e.preventDefault();
        const picked = mentionMatches[mentionHiClamped];
        if (picked) completeMention(picked);
        return;
      }
    }
    if (matches.length) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setHi((h) => Math.min(h + 1, matches.length - 1));
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setHi((h) => Math.max(h - 1, 0));
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setMenuOffFor(draft);
        return;
      }
      if (e.key === "Tab") {
        e.preventDefault();
        complete(matches[hiClamped]);
        return;
      }
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        // An exact verb submits; a prefix completes to the highlighted row.
        if (browse || (slash && SLASH_COMMANDS.some((c) => c.name === slash.name))) submit();
        else complete(matches[hiClamped]);
        return;
      }
    }
    if (e.key === "ArrowUp" && !e.shiftKey && !e.altKey && !e.ctrlKey && !e.metaKey && draft === "" && !matches.length && !block) {
      if (onArrowUpEmpty?.()) {
        e.preventDefault();
        return;
      }
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      submit();
    }
  };

  const attachEnabled = typeof attach === "boolean" ? attach : (attach?.enabled ?? false);
  const attachDestination = typeof attach === "object" ? attach?.destination : undefined;
  const addSelected = (files: Iterable<File>) => {
    if (attachEnabled) addFiles(key, files, attachDestination);
  };

  const attachChips =
    attachItems.length > 0 ? (
      <AttachChips
        items={attachItems}
        t={translateAttachText}
        onRetry={(item) => retryAttach(key, item.key)}
        onRemove={(item, focus) => {
          pendingFocus.current = focus;
          removeAttach(key, item.key);
        }}
      />
    ) : null;

  const boxEl = (
    <>
    {libraryImage ? (
      <div className="col-span-full flex items-center gap-2 text-label mb-2" data-library-image-chip="">
        {libraryPin ? (
          <>
            <img src={libraryPin.preview} alt="" width={40} height={40} className="size-10 rounded object-cover" />
            <span className="min-w-0 truncate">{libraryPin.title}</span>
          </>
        ) : <span role="status">{t("Attaching image reference…")}</span>}
        <button type="button" className="lnk" disabled={!!block} onClick={() => removeLibraryImagePin(key, libraryImage)}>
          {t("Remove")}
        </button>
      </div>
    ) : null}
    <textarea
      ref={box}
      id={textareaId}
      value={draft}
      onChange={(e) => {
        setSkillsFor(null);
        setMentionOffFor(null);
        setCaret(e.target.selectionStart ?? e.target.value.length);
        setDraft(e.target.value);
      }}
      onClick={(e) => setCaret(e.currentTarget.selectionStart ?? e.currentTarget.value.length)}
      onKeyUp={(e) => setCaret(e.currentTarget.selectionStart ?? e.currentTarget.value.length)}
      onSelect={(e) => setCaret(e.currentTarget.selectionStart ?? e.currentTarget.value.length)}
      onKeyDown={onKey}
      rows={1}
      disabled={!!block}
      placeholder={t(block ? blockedPlaceholder : hasArtifactRef ? "Say what to change…" : (placeholder ?? ""))}
      aria-label={t(ariaLabel ?? "Message to Assistant")}
      aria-expanded={matches.length > 0 || mentionMatches.length > 0 || undefined}
      className={
        compact
          ? "app-chat-box app-chat-textarea"
          : "app-chat-textarea w-full resize-none bg-transparent text-body text-ink-100 placeholder:text-ink-500 outline-none disabled:opacity-50 min-h-[2.75rem] max-h-40 overflow-y-auto"
      }
    />
    </>
  );

  const attachInput = attachEnabled ? (
    <input
      ref={fileInput}
      type="file"
      multiple
      className="sr-only"
      aria-hidden="true"
      tabIndex={-1}
      onChange={(e) => {
        addSelected(Array.from(e.target.files ?? []));
        e.target.value = "";
      }}
    />
  ) : null;
  // CAD-2425: the control is named beside its icon (the approved
  // chat-polish footer row); CSS shows the name from 640px.
  const attachControl = (labelled: boolean) => attach ? (
    <button
      type="button"
      className="app-chat-attach"
      aria-label={attachEnabled ? t("Attach files") : `${t("Attach files unavailable")}: ${typeof attach === "object" ? attach.reason : t("unavailable")}`}
      title={attachEnabled
        ? t("Attach files (text or PDF 10 MiB, video 20 MiB)")
        : typeof attach === "object" ? attach.reason : t("Attach is unavailable")}
      disabled={!!block || !attachEnabled}
      onClick={() => fileInput.current?.click()}
    >
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="M21 12.5l-8.5 8.5a5.5 5.5 0 01-7.8-7.8l9-9a3.67 3.67 0 015.2 5.2l-8.5 8.5a1.83 1.83 0 01-2.6-2.6l7.8-7.8" />
      </svg>
      {labelled && <span className="app-chat-attach-label" aria-hidden="true">{t("Attach file")}</span>}
    </button>
  ) : null;
  const sendHeld = unresolvedHint(attachHold(key), translateAttachText);
  // A reservation from a previous submit (even one made before a
  // remount) keeps Send off until its settlement releases it. An
  // unresolved original submission holds Send too, until its explicit
  // Retry or Discard.
  const reserved = cell.reserved;
  const imagePending = libraryImage?.state === "pending";
  const sendBlocked = !!block || !draft.trim() || (!browse && (sendHeld !== null || reserved || originalUnresolved || imagePending));
  const sendTitle = browse ? "Run command" : imagePending ? "Attaching image reference…" : sendHeld ?? (reserved ? "Sending…" : originalUnresolved ? "Retry or discard the previous message first" : null);

  // Drop handling is scoped to the composer surface itself — the same
  // selection path and limits as the picker; a drag that only crosses
  // the composer never navigates or uploads elsewhere.
  const dropProps = attachEnabled
    ? {
        onDragOver: (e: React.DragEvent) => {
          if (block) return;
          if (e.dataTransfer.types.includes("Files")) {
            e.preventDefault();
            setDropping(true);
          }
        },
        onDragLeave: (e: React.DragEvent) => {
          if (e.currentTarget.contains(e.relatedTarget as Node)) return;
          setDropping(false);
        },
        onDrop: (e: React.DragEvent) => {
          setDropping(false);
          if (block) return;
          if (e.dataTransfer.files.length > 0) {
            e.preventDefault();
            addSelected(Array.from(e.dataTransfer.files));
          }
        },
      }
    : {};

  const refsRow = refs.length > 0 ? (
        <div className="refsrow" aria-label={t("cited rows")} data-composer-refs>
          <span className="text-micro text-ink-500">{t("re:")}</span>
          {refs.map((r) => (
            <button
              key={`${r.kind}:${r.id}`}
              type="button"
              className="refchip num"
              title={`${t("remove the")} ${r.kind}:${r.id} ${t("reference")}`}
              onClick={() => setRefs(refs.filter((x) => x !== r))}
            >
              {askLabel(r)} <span aria-hidden>×</span>
            </button>
          ))}
        </div>
      
  ) : null;

  if (compact) {
    return (
      <form
        className={className}
        {...formProps}
        data-dropping={dropping || undefined}
        {...dropProps}
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        {refsRow}
        {attachChips}
        <div className="app-chat-composer" data-density="compact">
          {textareaId && (
            <label className="sr-only" htmlFor={textareaId}>
              {t(ariaLabel ?? "Message to Assistant")}
            </label>
          )}
          {boxEl}
          <div className="app-chat-composer-tools">
            {attachInput}
            {attachControl(true)}
            {actions && actions.length > 0 && <ComposerActions actions={actions} disabled={!!block} />}
            {stop ? <IconStop onStop={stop.onStop} /> : <IconSend disabled={sendBlocked} title={t(sendTitle ?? "Send")} />}
          </div>
        </div>
      </form>
    );
  }

  return (
    <form
      className={className}
      data-blocked={block ? "" : undefined}
      data-dropping={dropping || undefined}
      {...formProps}
      {...dropProps}
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      {block && <div className="text-label text-ink-300 mb-2 font-medium">{t("Read-only conversation")}</div>}
      {skillPickerOpen && (
        <WorkspaceSkillPicker
          onSelect={(name) => {
            setDraft(`/skill:${name} ${browse[1] ?? ""}`);
            setSkillsFor(null);
            box.current?.focus();
          }}
          onClose={(restoreFocus) => {
            setSkillsFor(null);
            if (restoreFocus) box.current?.focus();
          }}
        />
      )}
      {matches.length > 0 && (
        <ul className="slashmenu" role="listbox" aria-label={t("slash commands")}>
          {matches.map((c, i) => (
            <li key={c.name} role="option" aria-selected={i === hiClamped}>
              <button
                type="button"
                className={`slashrow${i === hiClamped ? " on" : ""}`}
                onMouseEnter={() => setHi(i)}
                onClick={() => complete(c)}
              >
                <span className="num text-accent shrink-0">/{c.name}</span>
                {c.arg && <span className="num text-ink-500 shrink-0">{c.arg}</span>}
                <span className="flex-1 min-w-0 truncate text-ink-500 text-left">{t(c.blurb)}</span>
                <span
                  className={`chip shrink-0 ${
                    c.kind === "read"
                      ? "bg-ink-800 text-ink-500"
                      : c.kind === "set"
                        ? "bg-info/10 text-info"
                        : "bg-warn/10 text-warn"
                  }`}
                >
                  {t(c.kind)}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
      {mentionMatches.length > 0 && (
        <MentionPicker
          candidates={mentionMatches}
          highlighted={mentionHiClamped}
          onHighlight={setMentionHi}
          onSelect={completeMention}
        />
      )}
      {refsRow}
      {attachChips}
      {boxEl}
      {/* CAD-2425: one footer row — attach, optional /note toggle, keyboard hint, then send. */}
      <div className="composer-foot" data-composer-foot="">
        {attachInput}
        {attachControl(true)}
        {showSilentToggle && onSilentChange && (
          <button
            type="button"
            className="composer-silent-toggle"
            aria-pressed={silent}
            disabled={!!block}
            title={t("Silent note — post without notifying members or waking agents")}
            onClick={() => onSilentChange(!silent)}
          >
            <span>/note</span>
          </button>
        )}
        {/* A held send's reason is on the failed file's own row; this
            line is only for a blocked (read-only) composer. */}
        {block ? (
          <p className="text-micro text-ink-500 min-w-0 flex-1 break-words" data-composer-block="">
            {t(block)}
          </p>
        ) : (
          <>
            <span className="flex-1" />
            <p className="composer-hint" data-composer-hint>
              {t("Enter to send · Shift+Enter for a new line")}
            </p>
          </>
        )}
        {stop ? <IconStop onStop={stop.onStop} /> : <IconSend disabled={sendBlocked} title={t(sendTitle ?? (slash ? "Run command" : "Send"))} />}
      </div>
    </form>
  );
}
