import Button from "../../../ui/Button";
import type { ContentDoc } from "../campaignGrammar";
import type { SmtpBinding } from "../sendClient";
import type { CampaignTab, FixTarget, ReadinessItem } from "./readiness";

/** Jump to a panel on the Overview and move focus into it. */
export function focusAnchor(id: string) {
  const el = document.getElementById(id);
  if (!el) return;
  el.scrollIntoView?.({ block: "start" });
  el.focus({ preventScroll: true });
}

/**
 * Overview: the ready-to-send checklist (driven by the same readiness
 * list that gates Prepare send) beside a short summary. Each unmet row
 * carries a Review/Fix link to the tab or panel that resolves it.
 */
export default function OverviewPane({
  items,
  doc,
  binding,
  audienceLabel,
  onTab,
}: {
  items: ReadinessItem[];
  doc: ContentDoc | null;
  binding: SmtpBinding | null | undefined;
  audienceLabel: string;
  onTab: (tab: CampaignTab) => void;
}) {
  const done = items.filter((item) => item.done).length;
  const go = (fix: FixTarget) => (fix.kind === "tab" ? onTab(fix.tab) : focusAnchor(fix.id));
  return (
    <div className="crm-cgrid">
      <section aria-label="Ready to send" className="card px-4 py-4 grid gap-2" data-checklist>
        <h4 className="text-cardtitle font-medium text-ink-100">
          Ready to send{" "}
          <span className="text-label text-ink-400 font-normal">
            {done} of {items.length}
          </span>
        </h4>
        <ul className="crm-ready" aria-label="Send checklist">
          {items.map((item) => (
            <li key={item.key} data-item={item.key} data-done={item.done || undefined}>
              <span className="crm-tick" data-done={item.done || undefined} aria-hidden="true">
                {item.done ? "✓" : "!"}
              </span>
              <span>
                {item.label}
                <span className="sr-only">{item.done ? " — done" : " — not done"}</span>
              </span>
              {!item.done && item.fix !== null && (
                <Button
                  size="sm"
                  className="crm-ready-go"
                  onClick={() => go(item.fix as FixTarget)}
                >
                  {item.fix.label}
                </Button>
              )}
            </li>
          ))}
        </ul>
      </section>
      <section aria-label="Campaign summary" className="card px-4 py-4 grid gap-2">
        <h4 className="text-cardtitle font-medium text-ink-100">Summary</h4>
        <dl className="sdetail" data-content-summary>
          <div>
            <dt>Subject</dt>
            <dd>{doc?.subject ?? "—"}</dd>
          </div>
          <div>
            <dt>From</dt>
            <dd>
              {binding === undefined
                ? "Reading…"
                : binding === null
                  ? "No sender connected"
                  : `${binding.sender.name} <${binding.sender.address}>`}
            </dd>
          </div>
          <div>
            <dt>Audience</dt>
            <dd>{audienceLabel}</dd>
          </div>
          <div>
            <dt>Version</dt>
            <dd>
              {doc === null
                ? "Not drafted yet"
                : `r${doc.revision} · ${
                    doc.approval.valid && doc.approval.revision === doc.revision
                      ? "approved"
                      : "not approved"
                  }`}
            </dd>
          </div>
        </dl>
      </section>
    </div>
  );
}
