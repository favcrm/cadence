/**
 * CAD-2543: the canvas body of a record view, a read-only email reader. Its
 * content is the stored `detail` card's own (no request). Every value is
 * untrusted third-party text, shown as plain text: no links, no HTML, no
 * Markdown. Ask and Draft reply fill the composer; nothing sends.
 */
import { useLocale, useTranslate } from "../../lib/locale";
import { IconClose } from "../../ui/icons";
import { inPrompt } from "../home/chatCards";
import { appendComposerDraft } from "../home/composerFill";
import { composerDraftState, requestComposerFocus, setComposerDraft } from "../app-shell/chat/composerStore";
import type { CanvasTab } from "./canvasStore";
import type { RecordViewRef } from "./recordView";

export default function RecordViewBody({ tab, view, composerKey, sheet, expanded, canExpand, onExpand, onClose }: {
  tab: CanvasTab;
  view: RecordViewRef;
  composerKey: string | null;
  sheet: boolean;
  expanded: boolean;
  canExpand: boolean;
  onExpand: () => void;
  onClose: () => void;
}) {
  const t = useTranslate();
  const { formatDate } = useLocale();
  const d = view.detail;
  const sender = d.name || d.address;
  const subject = inPrompt(d.title);
  const when = d.time === null ? "" : d.time.instant === null ? d.time.text : formatDate(d.time.instant, { year: "numeric", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
  const fill = (text: string) => {
    if (!composerKey) return;
    setComposerDraft(composerKey, appendComposerDraft(composerDraftState(composerKey).text, text));
    requestComposerFocus(composerKey);
    if (sheet) onClose();
  };
  const ask = () => fill(t("Summarise the email '{subject}' from {name}").replace("{subject}", () => subject).replace("{name}", () => inPrompt(sender)));
  const reply = () => fill(t("Draft a reply to {name} about '{subject}'").replace("{name}", () => inPrompt(sender)).replace("{subject}", () => subject));
  const source = [t("From {source}").replace("{source}", () => d.source), ...(d.account ? [d.account] : []), t("read-only")].join(" · ");
  const rows: [string, string][] = [
    [t("From"), d.address && d.name ? `${d.name} <${d.address}>` : sender],
    [t("To"), d.to],
    [t("Cc"), d.cc],
    [t("Date"), when],
    [t("Labels"), [...(d.star ? ["★"] : []), ...d.chips.map((c) => c.text)].join(", ")],
  ];
  return (
    <>
      <header className="cv-head">
        {sheet ? (
          <button type="button" className="cv-iconbtn cv-back" onClick={onClose} aria-label={t("Back to chat")}>
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
              <path d="m15 18-6-6 6-6" />
            </svg>
          </button>
        ) : null}
        <span className="cv-title">
          <b title={tab.title}>{tab.title}</b>
          <span className="cv-appview-sub" title={source}>{source}</span>
        </span>
        {!sheet ? (
          <>
            <button type="button" className="cv-iconbtn" onClick={onExpand} disabled={!canExpand} aria-pressed={expanded} aria-label={t("Expand canvas")} title={t("Expand canvas")}>
              <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
                {expanded ? <path d="M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5" /> : <path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5" />}
              </svg>
            </button>
            <button type="button" className="cv-iconbtn" onClick={onClose} aria-label={t("Close canvas")}>
              <IconClose size={12} />
            </button>
          </>
        ) : null}
      </header>
      <div className="cv-body cv-record-body" data-canvas-body data-record-view>
        <dl className="rv-meta">
          {rows.filter(([, value]) => value !== "").map(([label, value]) => (
            <div key={label} className="rv-metarow">
              <dt>{label}</dt>
              <dd>{value}</dd>
            </div>
          ))}
        </dl>
        <h2 className="rv-subject">{d.title}</h2>
        <div className="rv-text">{d.body}</div>
        {d.truncated ? <p className="rv-trunc" data-record-truncated>{t("…truncated")}</p> : null}
        {d.attachments.length > 0 ? (
          <ul className="rv-files" aria-label={t("Attachments")}>
            {d.attachments.map((name, i) => (
              <li key={i} className="rv-file" title={name}>{name}</li>
            ))}
          </ul>
        ) : null}
        <div className="rv-acts">
          <button type="button" className="btn btn-ghost" disabled={!composerKey} onClick={ask}>{t("Ask about this")}</button>
          <button type="button" className="btn btn-ghost" disabled={!composerKey} onClick={reply}>{t("Draft reply")}</button>
        </div>
      </div>
      <footer className="cv-foot">
        <span className="cv-foot-ro" data-record-source>{source}</span>
      </footer>
    </>
  );
}
