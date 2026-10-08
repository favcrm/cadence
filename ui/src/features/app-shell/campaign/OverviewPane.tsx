import Button from "../../../ui/Button";
import type { ContentDoc } from "../campaignGrammar";
import Link from "../../../ui/Link";
import type { SmtpBinding } from "../sendClient";
import { EMAIL_SENDING_HREF, isHostedTransport, sendingFrom } from "../../settings/emailSendingView";
import type { CampaignTab, FixTarget, ReadinessItem } from "./readiness";

/** Jump to a panel on the Overview and move focus into it. */
export function focusAnchor(id: string) {
  const el = document.getElementById(id);
  if (!el) return;
  const disclosure = el.closest("details");
  if (disclosure) disclosure.open = true;
  el.scrollIntoView?.({ block: "start" });
  el.focus({ preventScroll: true });
}

/** Compact summary plus a single prioritized next task. Guarded approval,
 * test and final-send actions stay in their own separate panels below. */
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
  const next = items.find((item) => !item.done && item.fix !== null);
  const from = sendingFrom(binding);
  const go = (fix: Exclude<FixTarget, { kind: "href" }>) =>
    fix.kind === "tab" ? onTab(fix.tab) : focusAnchor(fix.id);
  return (
    <div className="crm-cgrid" data-campaign-overview>
      <section aria-label="Next task" className="card px-4 py-4 grid gap-2" data-next-task>
        <h4 className="text-cardtitle font-medium text-ink-100">Next task</h4>
        {next === undefined ? (
          <>
            <p className="text-label text-ink-200">All test and review prerequisites are recorded.</p>
            <p className="text-micro text-ink-500">Final send still requires a separate guarded operator review.</p>
            <Button size="sm" className="justify-self-start" onClick={() => focusAnchor("cmp-final-send")}>
              Review final send
            </Button>
          </>
        ) : (
          <>
            <p className="text-label text-ink-200">{next.label}</p>
            {next.reason && <p className="text-micro text-ink-500">Requires {next.reason}.</p>}
            {next.fix?.kind === "href" ? (
              <Button size="sm" className="justify-self-start" href={next.fix.href}>{next.fix.label}</Button>
            ) : next.fix !== null ? (
              <Button size="sm" className="justify-self-start" onClick={() => go(next.fix as Exclude<FixTarget, { kind: "href" }>)}>
                {next.fix.label}
              </Button>
            ) : null}
          </>
        )}
      </section>
      <section aria-label="Campaign summary" className="card px-4 py-4 grid gap-2">
        <h4 className="text-cardtitle font-medium text-ink-100">Campaign summary</h4>
        <dl className="sdetail" data-content-summary>
          <div><dt>Audience</dt><dd>{audienceLabel}</dd></div>
          <div>
            <dt>Sender</dt>
            <dd>{binding === undefined ? "Checking…" : binding === null || from === null ? <Link href={EMAIL_SENDING_HREF}>No sender connected · Set up</Link> : `${binding.sender.name} <${from}>${isHostedTransport(binding) ? " · via AgenticOS" : ""}`}</dd>
          </div>
          <div><dt>Email</dt><dd>{doc === null ? "Not drafted" : doc.subject}</dd></div>
          <div>
            <dt>Content</dt>
            <dd>{doc === null ? "No saved revision" : `r${doc.revision} · ${doc.approval.valid && doc.approval.revision === doc.revision ? "content approved only" : "needs content review"}`}</dd>
          </div>
        </dl>
      </section>
    </div>
  );
}
