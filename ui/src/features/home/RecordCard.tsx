import { useContext, useEffect, useRef, useState } from "react";
import { useLocale, useTranslate } from "../../lib/locale";
import { TurnCard, type CardDensity } from "../../ui/TurnCard";
import {
  dayKey,
  parseRecordRef,
  plainText,
  recordLayout,
  type LabeledValue,
  type RecordCardModel,
  type RoleMessage,
  type RoleRow,
  type TimeValue,
} from "./chatCards";
import { ComposerFillContext } from "./composerFill";
import type { ThreadEntry } from "./thread";

/**
 * CAD-2443: the host's `record_ref` card, drawn from the payload alone as
 * a list, a message thread or a key/value table. Every value is plain text
 * (no Markdown, no links) and capped; a payload that does not parse or
 * lay out shows the entry's text line, never a half card.
 *
 * CAD-2458: header (toolkit, tool, account, count), "Name <address>" split,
 * mapped labels, unread dot, hover/focus row actions and tap-to-expand. A row
 * action only fills the composer; it never sends.
 */
const DIRECTION: Record<string, string> = { in: "Received", inbound: "Received", out: "Sent", outbound: "Sent" };

/** A value placed inside a composer prompt: one line, capped. */
const inPrompt = (value: string) => plainText(value.replace(/\s+/g, " "), 120);

function Pairs({ pairs }: { pairs: LabeledValue[] }) {
  return (
    <dl className="recordcard-pairs">
      {pairs.map((p, i) => (
        <div key={i} className="recordcard-pair">
          <dt>{p.label}</dt>
          <dd>{p.value}</dd>
        </div>
      ))}
    </dl>
  );
}

function Time({ value }: { value: TimeValue }) {
  const { formatRelativeTime } = useLocale();
  const shown = value.instant === null ? value.text : formatRelativeTime(value.instant);
  return (
    <time className="recordcard-time" dateTime={value.instant === null ? undefined : new Date(value.instant).toISOString()} title={value.text}>
      {shown}
    </time>
  );
}

/** A thread message's clock time; the text as given when it does not parse. */
function Clock({ value }: { value: TimeValue }) {
  const { formatDate } = useLocale();
  const shown = value.instant === null ? value.text : formatDate(value.instant, { hour: "2-digit", minute: "2-digit" });
  return (
    <time className="recordcard-time" dateTime={value.instant === null ? undefined : new Date(value.instant).toISOString()} title={value.text}>
      {shown}
    </time>
  );
}

function RowItem({ row, replyable }: { row: RoleRow; replyable: boolean }) {
  const t = useTranslate();
  const fill = useContext(ComposerFillContext);
  const [open, setOpen] = useState(false);
  const subject = inPrompt(row.subtitle || row.snippet || row.name);
  const ask = () => fill?.(t("Summarise the thread '{subject}'").replace("{subject}", () => subject));
  const reply = () =>
    fill?.(t("Draft a reply to {name} about '{subject}'").replace("{name}", () => inPrompt(row.name)).replace("{subject}", () => subject));
  return (
    <li className="recordcard-row" data-unread={row.unread || undefined} data-open={open || undefined}>
      <button type="button" className="recordcard-rowbtn" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
        <span className="recordcard-dot" aria-hidden />
        <span className="recordcard-who">
          {row.name && <b className="recordcard-name">{row.name}</b>}
          {row.address && <small className="recordcard-address">{row.address}</small>}
        </span>
        <span className="recordcard-when">
          {row.star && <span className="recordcard-star" role="img" aria-label={t("Important")}>★</span>}
          {row.time && <Time value={row.time} />}
          {row.amount && <span className="recordcard-amount">{row.amount}</span>}
        </span>
        {row.subtitle && <span className="recordcard-subject">{row.subtitle}</span>}
        {row.snippet && <span className="recordcard-snippet">{row.snippet}</span>}
        {row.unread && <span className="recordcard-sr">{t("Unread")}</span>}
        {row.chips.length > 0 && (
          <span className="recordcard-chips">
            {row.chips.map((c, j) => (
              <span key={j} className="recordcard-chip" data-muted={c.muted || undefined}>{c.text}</span>
            ))}
          </span>
        )}
      </button>
      {fill && (
        <span className="recordcard-acts">
          <button type="button" className="recordcard-act" onClick={ask}>
            <span aria-hidden>↗</span> {t("Ask about this")}
          </button>
          {replyable && row.name && (
            <button type="button" className="recordcard-act" onClick={reply}>
              <span aria-hidden>↩</span> {t("Reply")}
            </button>
          )}
        </span>
      )}
    </li>
  );
}

export function dayLabel(
  day: string,
  instant: number,
  t: (key: string) => string,
  formatDate: (v: number, o?: Intl.DateTimeFormatOptions) => string,
  now: number = Date.now(),
): string {
  if (day === dayKey(now)) return t("Today");
  if (day === dayKey(now - 86_400_000)) return t("Yesterday");
  const sameYear = new Date(instant).getFullYear() === new Date(now).getFullYear();
  return formatDate(instant, sameYear ? { month: "short", day: "numeric" } : { year: "numeric", month: "short", day: "numeric" });
}

function Bubbles({ messages }: { messages: RoleMessage[] }) {
  const t = useTranslate();
  const { formatDate } = useLocale();
  let lastDay: string | null = null;
  return (
    <ul className="recordcard-thread recordcard-bubbles">
      {messages.map((m, i) => {
        const separator = m.day !== null && m.day !== lastDay && m.time?.instant != null;
        if (m.day !== null) lastDay = m.day;
        return (
          <li key={i} className="recordcard-msg" data-own={m.own || undefined} data-day-break={separator || undefined}>
            {separator && m.day !== null && m.time?.instant != null && (
              <span className="recordcard-day" data-day={m.day}>{dayLabel(m.day, m.time.instant, t, formatDate)}</span>
            )}
            <span className="recordcard-bubble">
              <span className="recordcard-text">{m.text}</span>
              {(m.actor || m.time || m.approved) && (
                <span className="recordcard-meta">
                  {m.actor}
                  {m.actor && m.time ? " · " : ""}
                  {m.time && <Clock value={m.time} />}
                  {m.approved && <span className="recordcard-tag">{t("sent by assistant · approved")}</span>}
                </span>
              )}
            </span>
          </li>
        );
      })}
    </ul>
  );
}

/** Header: toolkit (bold) · tool · account, a count pill and the plain-text note. */
function Head({ model, count, contact }: { model: RecordCardModel; count: number | null; contact: string | null }) {
  const t = useTranslate();
  const { formatNumber } = useLocale();
  const fill = useContext(ComposerFillContext);
  const name = model.toolkitLabel ?? model.toolkit;
  const account = model.account;
  const sub = [
    ...(contact !== null ? [contact] : model.toolLabel ? [model.toolLabel] : []),
    ...(account ? [contact !== null ? t("from {account}").replace("{account}", () => account) : account] : []),
  ];
  const pill =
    count === null
      ? null
      : model.total !== null && model.total > count
        ? t("{n} of {total}").replace("{n}", formatNumber(count)).replace("{total}", formatNumber(model.total))
        : model.view === "thread"
          ? t("{n} messages").replace("{n}", formatNumber(count))
          : formatNumber(count);
  const note = t("Content from {toolkit} is shown as plain text").replace("{toolkit}", () => name);
  const replyTo = contact ? t("Draft a reply to {name}").replace("{name}", () => inPrompt(contact)) : t("Draft a reply");
  const [noteOpen, setNoteOpen] = useState(false);
  const wrap = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!noteOpen) return;
    const outside = (e: Event) => {
      if (!wrap.current?.contains(e.target as Node)) setNoteOpen(false);
    };
    const escape = (e: KeyboardEvent) => {
      if (e.key === "Escape") setNoteOpen(false);
    };
    document.addEventListener("pointerdown", outside);
    document.addEventListener("keydown", escape);
    return () => {
      document.removeEventListener("pointerdown", outside);
      document.removeEventListener("keydown", escape);
    };
  }, [noteOpen]);
  return (
    <div className="recordcard-headwrap" ref={wrap}>
      <div className="recordcard-head">
        <span className="recordcard-ic" aria-hidden>{[...name][0]?.toUpperCase() ?? "·"}</span>
        <span className="recordcard-headname">{name}</span>
        {!contact && sub.length > 0 && <span className="recordcard-headsub">{sub.map((s) => ` · ${s}`).join("")}</span>}
        {model.view === "thread" && fill && (
          <span className="recordcard-hact">
            <button type="button" className="recordcard-act" onClick={() => fill(replyTo)}>
              <span aria-hidden>↩</span> {t("Reply")}
            </button>
          </span>
        )}
        {pill !== null && <span className="recordcard-count" data-record-count>{pill}</span>}
        <button
          type="button"
          className="recordcard-info"
          aria-label={note}
          aria-expanded={noteOpen}
          title={note}
          onClick={() => setNoteOpen((o) => !o)}
        >
          i
        </button>
      </div>
      {contact && sub.length > 0 && <p className="recordcard-headline">{sub.join(" · ")}</p>}
      {noteOpen && <p className="recordcard-note" role="note">{note}</p>}
    </div>
  );
}

function MoreFooter({ model, shown }: { model: RecordCardModel; shown: number }) {
  const t = useTranslate();
  const fill = useContext(ComposerFillContext);
  const tool = inPrompt(model.toolLabel ?? model.toolkitLabel ?? model.toolkit);
  const text =
    model.total !== null && model.total > shown
      ? t("Showing {n} of {total}").replace("{n}", String(shown)).replace("{total}", String(model.total))
      : t("Showing {n}").replace("{n}", String(shown));
  return (
    <p className="recordcard-foot" data-record-truncated>
      <span>{text}</span>
      {fill && (
        <>
          <span aria-hidden>·</span>
          <button type="button" className="recordcard-act" onClick={() => fill(t("Show me more results from {tool}").replace("{tool}", () => tool))}>
            {t("Ask for more")}
          </button>
        </>
      )}
    </p>
  );
}

export function RecordCard({ entry, density = "full" }: { entry: ThreadEntry; density?: CardDensity }) {
  const t = useTranslate();
  const model = parseRecordRef(entry);
  const layout = model ? recordLayout(model, { yes: t("Yes"), no: t("No") }) : null;
  if (!model || !layout) return <p className="text-micro text-ink-500 break-words chatcard-line">· {plainText(entry.text, 500)}</p>;
  const count =
    layout.view === "list" || layout.view === "role-list"
      ? layout.rows.length
      : layout.view === "thread" || layout.view === "role-thread"
        ? layout.messages.length
        : null;
  const contact = layout.view === "role-thread" ? layout.contact || null : null;
  return (
    <TurnCard
      kind="record"
      className="card chatcard"
      bodyClassName="chatcard-body"
      density={density}
      data-kind="record_ref"
      data-record-view={layout.view}
      label={t("Record card")}
    >
      <Head model={model} count={count} contact={contact} />
      {layout.view === "list" && (
        <ul className="recordcard-list">
          {layout.rows.map((row, i) => (
            <li key={i}>
              <Pairs pairs={row} />
            </li>
          ))}
        </ul>
      )}
      {layout.view === "role-list" && (
        <ul className="recordcard-list recordcard-rows">
          {layout.rows.map((r, i) => (
            <RowItem key={i} row={r} replyable={model.roles?.snippet !== undefined} />
          ))}
        </ul>
      )}
      {layout.view === "role-thread" && <Bubbles messages={layout.messages} />}
      {layout.view === "thread" && (
        <ul className="recordcard-thread">
          {layout.messages.map((m, i) => (
            <li key={i} data-outbound={m.outbound || undefined}>
              <span className="recordcard-meta">
                {[t(DIRECTION[m.sender.toLowerCase()] ?? m.sender), m.time].filter(Boolean).join(" · ")}
              </span>
              <span className="recordcard-text">{m.text}</span>
            </li>
          ))}
        </ul>
      )}
      {layout.view === "table" && <Pairs pairs={layout.pairs} />}
      {model.truncated && <MoreFooter model={model} shown={count ?? model.items.length} />}
    </TurnCard>
  );
}
