import { useTranslate } from "../../lib/locale";
import { TurnCard, type CardDensity } from "../../ui/TurnCard";
import { parseRecordRef, plainText, recordLayout, type LabeledValue } from "./chatCards";
import type { ThreadEntry } from "./thread";

/**
 * CAD-2443: the host's `record_ref` card, drawn from the payload alone as
 * a list, a message thread or a key/value table. Every value is plain text
 * (no Markdown, no links) and capped; a payload that does not parse or
 * lay out shows the entry's text line, never a half card.
 */
const DIRECTION: Record<string, string> = { in: "Received", inbound: "Received", out: "Sent", outbound: "Sent" };

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

export function RecordCard({ entry, density = "full" }: { entry: ThreadEntry; density?: CardDensity }) {
  const t = useTranslate();
  const model = parseRecordRef(entry);
  const layout = model ? recordLayout(model, { yes: t("Yes"), no: t("No") }) : null;
  if (!model || !layout) return <p className="text-micro text-ink-500 break-words chatcard-line">· {plainText(entry.text, 500)}</p>;
  return (
    <TurnCard
      kind="record"
      className="card chatcard"
      bodyClassName="chatcard-body"
      density={density}
      data-kind="record_ref"
      data-record-view={layout.view}
      label={t("Record card")}
      kindLabel={t("Records")}
      subject={plainText(entry.text, 200)}
    >
      {layout.view === "list" && (
        <ul className="recordcard-list">
          {layout.rows.map((row, i) => (
            <li key={i}>
              <Pairs pairs={row} />
            </li>
          ))}
        </ul>
      )}
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
      {model.truncated && (
        <p className="text-micro text-ink-500 break-words chatcard-line" data-record-truncated>
          {t("More in {tool}").replace("{tool}", () => model.tool)}
        </p>
      )}
    </TurnCard>
  );
}
