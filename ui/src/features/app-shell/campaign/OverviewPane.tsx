import Button from "../../../ui/Button";
import { useLocale } from "../../../lib/locale";
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
  const { t, formatNumber } = useLocale();
  const [who, ...countStatuses] = audienceLabel.split(" · ");
  const localizedWho = who === "All customers"
    ? t(who)
    : who.startsWith("Segment ")
      ? `${t("Segment")} ${who.slice("Segment ".length)}`
      : who.endsWith(" chosen customers")
        ? `${formatNumber(Number.parseInt(who, 10))} ${t("chosen customers")}`
        : who;
  const localizedCounts = countStatuses.map((countStatus) => countStatus === "not counted yet"
    ? t(countStatus)
    : countStatus.endsWith(" match")
      ? `${formatNumber(Number.parseInt(countStatus, 10))} ${t("match")}`
      : countStatus.endsWith(" can be emailed")
        ? `${formatNumber(Number.parseInt(countStatus, 10))} ${t("can be emailed")}`
        : countStatus);
  const localizedAudience = [localizedWho, ...localizedCounts].filter(Boolean).join(" · ");
  const next = items.find((item) => !item.done && item.fix !== null);
  const from = sendingFrom(binding);
  const go = (fix: Exclude<FixTarget, { kind: "href" }>) =>
    fix.kind === "tab" ? onTab(fix.tab) : focusAnchor(fix.id);
  return (
    <div className="crm-cgrid" data-campaign-overview>
      <section aria-label={t("Next task")} className="card px-4 py-4 grid gap-2" data-next-task>
        <h4 className="text-cardtitle font-medium text-ink-100">{t("Next task")}</h4>
        {next === undefined ? (
          <>
            <p className="text-label text-ink-200">{t("All test and review prerequisites are recorded.")}</p>
            <p className="text-micro text-ink-500">{t("Final send still requires a separate guarded operator review.")}</p>
            <Button size="sm" className="justify-self-start" onClick={() => focusAnchor("cmp-final-send")}>
              {t("Review final send")}
            </Button>
          </>
        ) : (
          <>
            <p className="text-label text-ink-200">{t(next.label)}</p>
            {next.reason && <p className="text-micro text-ink-500">{t("Requires")} {t(next.reason)}.</p>}
            {next.fix?.kind === "href" ? (
              <Button size="sm" className="justify-self-start" href={next.fix.href}>{t(next.fix.label)}</Button>
            ) : next.fix !== null ? (
              <Button size="sm" className="justify-self-start" onClick={() => go(next.fix as Exclude<FixTarget, { kind: "href" }>)}>
                {t(next.fix.label)}
              </Button>
            ) : null}
          </>
        )}
      </section>
      <section aria-label={t("Campaign summary")} className="card px-4 py-4 grid gap-2">
        <h4 className="text-cardtitle font-medium text-ink-100">{t("Campaign summary")}</h4>
        <dl className="sdetail" data-content-summary>
          <div><dt>{t("Audience")}</dt><dd>{localizedAudience}</dd></div>
          <div>
            <dt>{t("Sender")}</dt>
            <dd>{binding === undefined ? t("Checking…") : binding === null || from === null ? <Link href={EMAIL_SENDING_HREF}>{t("No sender connected · Set up")}</Link> : `${binding.sender.name} <${from}>${isHostedTransport(binding) ? ` · ${t("via AgenticOS")}` : ""}`}</dd>
          </div>
          <div><dt>{t("Email")}</dt><dd>{doc === null ? t("Not drafted") : doc.subject}</dd></div>
          <div>
            <dt>{t("Content")}</dt>
            <dd>{doc === null ? t("No saved revision") : `r${formatNumber(doc.revision)} · ${t(doc.approval.valid && doc.approval.revision === doc.revision ? "content approved only" : "needs content review")}`}</dd>
          </div>
        </dl>
      </section>
    </div>
  );
}
