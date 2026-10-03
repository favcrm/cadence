import Button from "../../../ui/Button";
import type { ContentDoc } from "../campaignGrammar";
import Link from "../../../ui/Link";
import type { SmtpBinding } from "../sendClient";
import {
  EMAIL_SENDING_HREF,
  isHostedTransport,
  PLATFORM_EMAIL_WAITING,
  PLATFORM_EMAIL_WAITING_NOTE,
  sendingFrom,
  type HostedSender,
} from "../../settings/emailSendingView";
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
  hostedSender,
  audienceLabel,
  onTab,
}: {
  items: ReadinessItem[];
  doc: ContentDoc | null;
  binding: SmtpBinding | null | undefined;
  /** CAD-1121: what the daemon says about the platform sender; null until read. */
  hostedSender?: HostedSender | null;
  audienceLabel: string;
  onTab: (tab: CampaignTab) => void;
}) {
  const done = items.filter((item) => item.done).length;
  const go = (fix: Exclude<FixTarget, { kind: "href" }>) =>
    fix.kind === "tab" ? onTab(fix.tab) : focusAnchor(fix.id);
  const from = sendingFrom(binding);
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
              {!item.done && item.fix !== null && item.fix.kind === "href" && (
                <Button size="sm" className="crm-ready-go" href={item.fix.href}>
                  {item.fix.label}
                </Button>
              )}
              {!item.done && item.fix !== null && item.fix.kind !== "href" && (
                <Button
                  size="sm"
                  className="crm-ready-go"
                  onClick={() => go(item.fix as Exclude<FixTarget, { kind: "href" }>)}
                >
                  {item.fix.label}
                </Button>
              )}
            </li>
          ))}
        </ul>
        <div className="text-label" data-sending-status={from === null ? "unset" : "set"}>
          {binding === undefined ? (
            <span className="text-ink-500">Checking the sender…</span>
          ) : from === null && hostedSender?.hosted && !hostedSender.available ? (
            <span className="text-ink-300" data-state="platform-waiting">
              {PLATFORM_EMAIL_WAITING}. {PLATFORM_EMAIL_WAITING_NOTE}
              <details className="text-ink-400" data-details="platform-sender">
                <summary className="cursor-pointer">Details</summary>
                Missing: the platform's sending address
                {hostedSender.missing ? ` (${hostedSender.missing})` : ""}.
              </details>
            </span>
          ) : from === null ? (
            <Link href={EMAIL_SENDING_HREF}>Set up sending →</Link>
          ) : (
            <span className="text-ink-200">
              Sending from {from}
              {isHostedTransport(binding) ? " via AgenticOS" : ""} <span aria-hidden="true">✓</span>
            </span>
          )}
        </div>
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
                : from === null
                  ? "No sender connected"
                  : `${binding?.sender.name ?? ""} <${from}>`.trim()}
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
