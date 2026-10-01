import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import type { ThreadRef } from "../../lib/types";
import Button from "../../ui/Button";
import { parseSlash, SLASH_COMMANDS, slashMatches, type SlashCommand } from "./master";
import { sendToMaster } from "./send";
import { newMessageId } from "./thread";

/**
 * The composer owns its draft: typing re-renders this form only, never
 * the thread above it. `seed` (an example ask) replaces the draft.
 *
 * CAD-551 slash UX: a draft beginning with `/` is a command, not a chat
 * message — the menu completes verbs while the first token is typed,
 * Tab/Enter completes, Esc dismisses, and a complete `/verb [arg]`
 * submits through `onCommand` (never to the thread).
 *
 * CAD-600: this is the floating dock — pinned to the panel's bottom by
 * its wrapper, the textarea growing with the draft up to ~8 lines and
 * scrolling past that (the CSS max-height), and the focus staying in
 * the box after a send.
 */
export default function Composer({
  block,
  seed,
  onCommand,
  onSent,
}: {
  block: string | null;
  seed: { text: string; n: number; refs?: ThreadRef[] };
  onCommand: (name: string, arg: string) => void;
  /** Enter or the button actually submitted — chat, slash, or an Ask-master draft. */
  onSent: () => void;
}) {
  const [draft, setDraft] = useState("");
  // CAD-574: an Ask-master seed carries the row's subject — chips shown
  // beside the draft, attached to `thread_send` as `refs` on send. A
  // removed chip removes the ref; a sent draft clears them all.
  const [refs, setRefs] = useState<ThreadRef[]>([]);
  const [hi, setHi] = useState(0);
  // Esc dismisses the menu for THIS verb text — the next keystroke
  // reopens it (or a cleared draft does).
  const [menuOffFor, setMenuOffFor] = useState<string | null>(null);
  const form = useRef<HTMLFormElement>(null);
  const box = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    if (seed.n > 0) {
      setDraft(seed.text);
      setRefs(seed.refs ?? []);
      if (!block) box.current?.focus();
    }
  }, [seed]);
  // CAD-600: the textarea grows with its content (auto-height), capped
  // by the CSS max-height — past ~8 lines it scrolls instead. A seeded
  // draft grows the same way, and a cleared one shrinks back.
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [draft]);

  const slash = parseSlash(draft);
  // The menu shows while the draft is a verb prefix ("/", "/st") — once
  // a space lands the arg is being typed and the menu stands down.
  const verbDraft = draft.startsWith("/") && !draft.includes(" ") ? draft.slice(1) : null;
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
    if (!body || block) return;
    // Pin before the pending row or command card paints, so the
    // follow effect scrolls the working indicator into view.
    onSent();
    const cmd = parseSlash(body);
    if (cmd) onCommand(cmd.name, cmd.arg);
    else sendToMaster(body, newMessageId(), refs);
    setDraft("");
    setRefs([]);
    // The dock keeps the focus: a click on Send must not strand the
    // caret on the button — the next message is one keystroke away.
    box.current?.focus();
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
  return (
    <form
      ref={form}
      className="card p-2.5 relative"
      data-composer
      data-blocked={block ? "" : undefined}
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      {block && <div className="text-label text-ink-300 mb-2 font-medium">Read-only conversation</div>}
      {matches.length > 0 && (
        <ul className="slashmenu" role="listbox" aria-label="slash commands">
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
                  {c.kind}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
      {refs.length > 0 && (
        <div className="refsrow" aria-label="cited rows" data-composer-refs>
          <span className="text-micro text-ink-500">re:</span>
          {refs.map((r) => (
            <button
              key={`${r.kind}:${r.id}`}
              type="button"
              className="refchip num"
              title={`remove the ${r.kind}:${r.id} reference`}
              onClick={() => setRefs((rs) => rs.filter((x) => x !== r))}
            >
              {r.kind}:{r.id} <span aria-hidden>×</span>
            </button>
          ))}
        </div>
      )}
      <textarea
        ref={box}
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={onKey}
        rows={2}
        disabled={!!block}
        placeholder={block ? "Read-only · Sending is unavailable" : "Ask Master about your team or plan the next job…"}
        aria-label="message to the master"
        aria-expanded={matches.length > 0 || undefined}
        className="w-full resize-none bg-transparent text-body text-ink-100 placeholder:text-ink-500 outline-none disabled:opacity-50 min-h-[2.75rem] max-h-40 overflow-y-auto"
      />
      <div className="flex items-center gap-2 mt-1.5">
        <p className="text-micro text-ink-500 min-w-0 flex-1 break-words" data-composer-block={block ? "" : undefined}>
          {block ?? (slash ? "Enter runs the command · Tab completes" : "Enter sends · Shift+Enter for a new line")}
        </p>
        <Button
          type="submit"
          variant="primary"
          size="sm"
          className="sendbtn shrink-0"
          disabled={!!block || !draft.trim()}
        >
          {slash ? "Run" : "Send"}
        </Button>
      </div>
    </form>
  );
}
