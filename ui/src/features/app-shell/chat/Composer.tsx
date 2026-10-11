import {
  useCallback,
  useEffect,
  useRef,
  useState,
  useSyncExternalStore,
  type KeyboardEvent,
} from "react";
import type { ThreadRef } from "../../../lib/types";
import { useLocale } from "../../../lib/locale";
import { parseSlash, SLASH_COMMANDS, slashMatches, type SlashCommand } from "../../home/master";
import { ATTACH_ACCEPT, attachMeta, unresolvedHint, type AttachDestination, type AttachItem } from "./attach";
import {
  addFiles,
  attachHold,
  attachItems as storedAttachItems,
  captureReady,
  composerDraftState,
  consumeSeed,
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
  /** CAD-1168: daemon-minted ids of the retained uploads that are `ready`. */
  attachments?: { id: string }[];
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
   * (`/new`) pass it too; without it every submission is a send.
   */
  onCommand?: (name: string, arg: string) => void;
  /** A real send completed validation — deliver `body` (with `refs`).
   *  The returned promise settles when the destination's pending row
   *  owns the envelope; a rejected promise keeps the draft and files. */
  onSend: (send: ComposerSend) => void | Promise<void>;
  /** Any submission (send or command) — pin/follow hooks, before the send paints. */
  onSubmitted?: () => void;
  /** Focus the box after a send (the Home dock does). */
  refocusAfterSend?: boolean;
  textareaId?: string;
  ariaLabel?: string;
  placeholder?: string;
  blockedPlaceholder?: string;
  className?: string;
  /** Extra form attributes — Home's `data-composer` hook is one. */
  formProps?: Record<string, string | undefined>;
  /** The shared Attach picker/drop/chips. The app shell enables it only
   *  from the live native projection; Home keeps its historical shape. */
  attach?: boolean | {
    enabled: boolean;
    reason: string;
    destination?: AttachDestination;
  };
  /** A submission that is neither a send nor a known command surfaces
   *  this one-line notice (the unsupported-slash refusal and the
   *  unresolved-attachment hold). */
  onNotice?: (text: string) => void;
  /** The original submitted envelope is unresolved (a failed create kept
   *  it in the pane's outbox): an ordinary new submission is refused
   *  until the operator retries or discards it. Typing, refs and files
   *  stay editable — the box is never disabled and nothing is silently
   *  discarded. */
  originalUnresolved?: boolean;
  /** The operator asked to settle the unresolved original: the pane
   *  reveals its recovery control. */
  onResolveOriginal?: () => void;
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

export default function Composer({
  density,
  block,
  storeKey,
  seed,
  onCommand,
  onSend,
  onSubmitted,
  refocusAfterSend,
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
}: ComposerProps) {
  const { t } = useLocale();
  const compact = density === "compact";
  const key = storeKey ?? "composer";
  // The draft cell is the ONE owner of text, refs, edit version and the
  // in-flight handoff reservation. The box renders this snapshot, so a
  // remount and a settlement can never leave a stale second copy.
  const subscribeDraftCell = useCallback((listener: () => void) => subscribeDraft(key, listener), [key]);
  const readDraftCell = useCallback(() => composerDraftState(key), [key]);
  const cell = useSyncExternalStore(subscribeDraftCell, readDraftCell);
  const draft = cell.text;
  const refs = cell.refs;
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
  const [hi, setHi] = useState(0);
  // Esc dismisses the menu for THIS verb text — the next keystroke
  // reopens it (or a cleared draft does).
  const [menuOffFor, setMenuOffFor] = useState<string | null>(null);
  const box = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    if (seed && seed.n > 0 && consumeSeed(key, seed)) {
      setComposerContent(key, seed.text, seed.refs ?? []);
      if (!block) box.current?.focus();
    }
  }, [seed]);
  // The textarea grows with its content (auto-height), capped by the
  // CSS max-height — past it the box scrolls. `resize:none` is the
  // mock's rule: no bottom-right grip in either density.
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [draft]);

  // Slash completion is a full-density extra (Home's commands); compact
  // mode routes slash verbs through onCommand without sending them
  // as ordinary chat.
  const menu = !compact && onCommand !== undefined;
  const slash = menu ? parseSlash(draft) : null;
  const verbDraft = menu && draft.startsWith("/") && !draft.includes(" ") ? draft.slice(1) : null;
  const matches =
    verbDraft !== null && draft !== menuOffFor ? slashMatches(verbDraft.toLowerCase()) : [];
  useEffect(() => setHi(0), [verbDraft]);
  const hiClamped = Math.min(hi, Math.max(0, matches.length - 1));

  const complete = (c: SlashCommand) => {
    setDraft(`/${c.name}${c.arg ? " " : ""}`);
    setMenuOffFor(null);
  };

  const submit = () => {
    const body = draft.trim();
    // `block` is also "not an operator" in an app pane — the box and
    // Send are disabled and no submission leaves the composer.
    if (!body || block) return;
    // An unresolved original submission owns this key: a new ordinary
    // submission is refused until the operator retries or discards it,
    // so the earlier intent can never be silently replaced. The newer
    // text, refs and files stay editable and are not discarded.
    if (originalUnresolved) {
      onNotice?.("The previous message was not sent — retry or discard it before sending again");
      onResolveOriginal?.();
      return;
    }
    // A queued file that is still uploading or that failed is never
    // silently dropped: the send waits until every row is ready or
    // removed, and the notice says exactly why.
    const hold = unresolvedHint(attachHold(key));
    if (hold !== null) {
      onNotice?.(hold);
      return;
    }
    // Pin before a pending row or command card paints, so a follow
    // effect scrolls the working indicator into view.
    onSubmitted?.();
    const cmd = parseSlash(body);
    if (cmd && !menu && onCommand !== undefined) {
      // A compact pane with its own commands (`/new`): exact verb.
      onCommand(cmd.name, cmd.arg);
    } else if (cmd && !menu) {
      // An unsupported slash verb is never forwarded as chat — the
      // composer refuses it in place. Full-density menus cannot reach
      // this: an exact verb already submitted above.
      onNotice?.(`Unsupported command "/${cmd.name}" — it was not sent`);
      return;
    } else if (cmd) {
      onCommand?.(cmd.name, cmd.arg);
    } else {
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
          body,
          refs,
          // Only ready rows ride a send — gating above already withheld
          // it while anything unresolved remained.
          attachments: ready.ids,
        });
      } catch {
        // A synchronous handoff failure must release the reservation,
        // not wedge the composer behind a handoff that never settled.
        settleDraft(reservation, false);
        return;
      }
      const settle = (accepted: boolean) => {
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
    if (e.nativeEvent.isComposing) return;
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
        if (slash && SLASH_COMMANDS.some((c) => c.name === slash.name)) submit();
        else complete(matches[hiClamped]);
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
      <div className="app-chat-attachrow" data-composer-attachments>
        {attachItems.map((a) => (
          <div
            key={a.key}
            role="group"
            className={`app-chat-filerow${a.status === "failed" ? " failed" : ""}`}
            data-attach-status={a.status}
            aria-label={`${a.name}: ${t(a.status)}${a.error ? ` — ${a.error}` : ""}`}
          >
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M5 3h9l5 5v13H5zM14 3v6h5M8 13h8M8 17h5" />
            </svg>
            <div className="app-chat-filename">
              <span>{a.name}</span>
              <small>
                {attachMeta(a.name, a.size)}
                {a.status === "uploading" ? ` · ${t("Uploading…")}` : ""}
              </small>
              {a.status === "failed" && a.error && (
                <small className="app-chat-upload-error" role="alert">
                  {a.error}
                </small>
              )}
            </div>
            {a.status === "failed" && !a.permanent && (
              <button type="button" className="lnk" onClick={() => retryAttach(key, a.key)}>
                {t("Retry")}
              </button>
            )}
            <button
              type="button"
              className="app-chat-remove"
              aria-label={`${t("Remove")} ${a.name}`}
              title={`${t("Remove")} ${a.name}`}
              onClick={() => removeAttach(key, a.key)}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M6 6l12 12M18 6L6 18" />
              </svg>
            </button>
          </div>
        ))}
      </div>
    ) : null;

  const boxEl = (
    <textarea
      ref={box}
      id={textareaId}
      value={draft}
      onChange={(e) => setDraft(e.target.value)}
      onKeyDown={onKey}
      rows={compact ? 1 : 2}
      disabled={!!block}
      placeholder={t(block ? blockedPlaceholder : (placeholder ?? ""))}
      aria-label={t(ariaLabel ?? "Message to Assistant")}
      aria-expanded={matches.length > 0 || undefined}
      className={
        compact
          ? "app-chat-box app-chat-textarea"
          : "app-chat-textarea w-full resize-none bg-transparent text-body text-ink-100 placeholder:text-ink-500 outline-none disabled:opacity-50 min-h-[2.75rem] max-h-40 overflow-y-auto"
      }
    />
  );

  const attachInput = attachEnabled ? (
    <input
      ref={fileInput}
      type="file"
      multiple
      accept={ATTACH_ACCEPT}
      className="sr-only"
      aria-hidden="true"
      tabIndex={-1}
      onChange={(e) => {
        addSelected(Array.from(e.target.files ?? []));
        e.target.value = "";
      }}
    />
  ) : null;
  const attachButton = attach ? (
    <button
      type="button"
      className="app-chat-attach"
      aria-label={attachEnabled ? t("Attach files") : `${t("Attach files unavailable")}: ${typeof attach === "object" ? attach.reason : "unavailable"}`}
      title={attachEnabled
        ? t("Attach files (txt, md, csv — up to 10 MiB each; PDF/image processing is not available yet)")
        : typeof attach === "object" ? attach.reason : "Attach is unavailable"}
      disabled={!!block || !attachEnabled}
      onClick={() => fileInput.current?.click()}
    >
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="M21 12.5l-8.5 8.5a5.5 5.5 0 01-7.8-7.8l9-9a3.67 3.67 0 015.2 5.2l-8.5 8.5a1.83 1.83 0 01-2.6-2.6l7.8-7.8" />
      </svg>
    </button>
  ) : null;
  const sendHeld = unresolvedHint(attachHold(key));
  // A reservation from a previous submit (even one made before a
  // remount) keeps Send off until its settlement releases it. An
  // unresolved original submission holds Send too, until its explicit
  // Retry or Discard.
  const reserved = cell.reserved;
  const sendBlocked = !!block || !draft.trim() || sendHeld !== null || reserved || originalUnresolved;
  const sendTitle = sendHeld ?? (reserved ? "Sending…" : originalUnresolved ? "Retry or discard the previous message first" : null);

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
            {attachButton}
            <IconSend disabled={sendBlocked} title={t(sendTitle ?? "Send")} />
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
                <span className="flex-1 min-w-0 truncate text-ink-500 text-left">{c.blurb}</span>
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
      {refs.length > 0 && (
        <div className="refsrow" aria-label={t("cited rows")} data-composer-refs>
          <span className="text-micro text-ink-500">re:</span>
          {refs.map((r) => (
            <button
              key={`${r.kind}:${r.id}`}
              type="button"
              className="refchip num"
              title={`${t("remove the")} ${r.kind}:${r.id} ${t("reference")}`}
              onClick={() => setRefs(refs.filter((x) => x !== r))}
            >
              {r.kind}:{r.id} <span aria-hidden>×</span>
            </button>
          ))}
        </div>
      )}
      {attachChips}
      {boxEl}
      <div className="flex items-center gap-2 mt-1.5">
        {attachInput}
        {attachButton}
        {/* A held send's reason is on the failed file's own row; this
            line is only for a blocked (read-only) composer. */}
        {block ? (
          <p className="text-micro text-ink-500 min-w-0 flex-1 break-words" data-composer-block="">
            {t(block)}
          </p>
        ) : (
          <span className="flex-1" />
        )}
        <IconSend disabled={sendBlocked} title={t(sendTitle ?? (slash ? "Run command" : "Send"))} />
      </div>
    </form>
  );
}
