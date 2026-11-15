import type { ReactNode } from "react";
import { useTranslate } from "../../src/lib/localeText";
import { Button } from "../../src/ui/Button";

export type StagedApprovalTone = "warn" | "info" | "fail";

export interface StagedApprovalBannerProps {
  title: string;
  description?: string;
  summary?: ReactNode;
  tone?: StagedApprovalTone;
  approveLabel?: string;
  discardLabel?: string;
  onApprove?: () => void;
  onDiscard?: () => void;
  pending?: boolean;
  approveDisabled?: boolean;
  actions?: ReactNode;
  className?: string;
}

/** Operator confirmation banner for staged external writes or gated actions. */
export function StagedApprovalBanner({
  title,
  description,
  summary,
  tone = "warn",
  approveLabel,
  discardLabel,
  onApprove,
  onDiscard,
  pending = false,
  approveDisabled = false,
  actions,
  className,
}: StagedApprovalBannerProps) {
  const t = useTranslate();
  const role = tone === "fail" ? "alert" : "region";

  return (
    <div
      role={role}
      aria-label={title}
      data-tone={tone}
      className={className ? `s-staged-banner ${className}` : "s-staged-banner"}
    >
      <div className="s-staged-banner-body">
        <div className="text-body font-medium text-[var(--color-ink)]">{title}</div>
        {description ? (
          <p className="text-label text-[var(--color-ink-2)]">{description}</p>
        ) : null}
        {summary ? (
          <div className="text-label text-[var(--color-ink-2)]">{summary}</div>
        ) : null}
      </div>
      {onDiscard || onApprove || actions ? (
        <div className="s-staged-banner-actions">
          {onDiscard ? (
            <Button size="sm" variant="ghost" disabled={pending} onClick={onDiscard}>
              {discardLabel ?? t("Discard")}
            </Button>
          ) : null}
          {onApprove ? (
            <Button
              size="sm"
              variant={tone === "fail" ? "danger" : "primary"}
              loading={pending}
              disabled={pending || approveDisabled}
              onClick={onApprove}
            >
              {approveLabel ?? t("Confirm")}
            </Button>
          ) : null}
          {actions}
        </div>
      ) : null}
    </div>
  );
}
