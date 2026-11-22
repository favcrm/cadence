import type { KeyboardEvent, MouseEvent, ReactNode } from "react";
import type { RailTone } from "./railModel";

export interface RailCardProps {
  /** Tone rendered beside the card status dot & label. */
  tone: RailTone;
  /** Keep working-agent green dot subtly pulsing when active. */
  live?: boolean;
  /** Status label beside the dot (e.g. Approval, Review, Blocked, Working, Completed). */
  statusLabel: string;
  /** Optional monogram badge shown before the actor/source name. */
  initials?: string;
  /** Actor or source rendered in the card header. */
  source: ReactNode;
  /** Relative age or timestamp shown at the top-right. */
  age?: string;
  /** Stretched clickable title inside `.rc-title`. */
  title: ReactNode;
  /** Short 1-2 line description below the title. */
  summary?: ReactNode;
  /** Left-aligned metadata footer (project, ticket id, count). */
  meta?: ReactNode;
  /** Highlights external/sensitive footer metadata in warning tone. */
  external?: boolean;
  /** Right-aligned passive cue (`Review`, `Inspect`, `Details`). Never a second button. */
  cue?: string;
  /** Stretched hit-target callback opening the drawer. */
  onOpen?: (event: MouseEvent<HTMLButtonElement>) => void;
  disabled?: boolean;
  done?: boolean;
  className?: string;
  titleClassName?: string;
  bodyClassName?: string;
  metaClassName?: string;
}

/**
 * Shared Card View surface used by both To do and Feed items.
 * Carries zero inline action buttons: clicking anywhere on the card
 * activates `.rc-title::after` to open its detail drawer first.
 */
export function RailCard({
  tone,
  live = false,
  statusLabel,
  initials,
  source,
  age,
  title,
  summary,
  meta,
  external = false,
  cue = "Details",
  onOpen,
  disabled = false,
  done = false,
  className,
  titleClassName,
  bodyClassName,
  metaClassName,
}: RailCardProps) {
  const onKeyDown = (event: KeyboardEvent<HTMLButtonElement>) => {
    if (disabled) return;
    if (event.key === " " && onOpen) {
      event.preventDefault();
      event.currentTarget.click();
    }
  };

  return (
    <div
      className={`rail-card${className ? ` ${className}` : ""}`}
      data-done={done ? "true" : undefined}
    >
      <div className="rc-head">
        <div className="rc-head-left">
          {initials ? <span className="rc-ava" aria-hidden="true">{initials}</span> : null}
          {initials ? <span className="rc-agent">{source}</span> : null}
          {initials ? <span className="rc-sep" aria-hidden="true">·</span> : null}
          <span className={`sdot ${live ? "live" : tone}`} aria-hidden="true" />
          <span className={`status-txt ${tone}`}>{statusLabel}</span>
          {!initials ? <span className="rc-sep" aria-hidden="true">·</span> : null}
          {!initials ? <span className="rc-source">{source}</span> : null}
        </div>
        {age ? <div className="rc-head-right num">{age}</div> : null}
      </div>

      <div className={`rc-body${bodyClassName ? ` ${bodyClassName}` : ""}`}>
        <h3 className="rc-heading">
          <button
            type="button"
            className={`rc-title${titleClassName ? ` ${titleClassName}` : ""}`}
            disabled={disabled}
            onClick={onOpen}
            onKeyDown={onKeyDown}
          >
            {title}
          </button>
        </h3>
        {summary != null ? (
          <div className={`rc-summary${metaClassName ? ` ${metaClassName}` : ""}`}>{summary}</div>
        ) : null}
      </div>

      {(meta != null || cue) ? (
        <div className="rc-foot">
          <span className={`rc-meta${external ? " ext" : ""}`}>{meta}</span>
          {cue ? (
            <span className="rc-cue" aria-hidden="true">
              {cue} <span className="rc-chev">›</span>
            </span>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
