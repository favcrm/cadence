import SafeLink from "../../ui/SafeLink";
import type { HomeNeed } from "./needs";

/** One row of a rail's inline menu (the `…` overflow and the Unfence
 *  reconcile choice share it). */
const MENU_ITEM =
  "needitem w-full text-left px-2.5 py-1.5 text-label text-ink-200 hover:bg-ink-800 rounded disabled:opacity-40 disabled:hover:bg-transparent";

const SNOOZE_24H = 86_400;
const SNOOZE_7D = 604_800;

function copy(text: string): Promise<void> {
  try {
    return navigator.clipboard.writeText(text);
  } catch (e) {
    return Promise.reject(e);
  }
}

/**
 * The `…` overflow (CAD-574): Open lands on the row's own link or its
 * issue page; Snooze and Dismiss are the operator's `needs_dismiss`
 * routes; Copy command keeps the row's fallback command one tap away;
 * Details (CAD-1216) shows the technical fields the card leaves out.
 */
export function NeedMenu({
  need,
  block,
  busy,
  onOpenIssue,
  onDecide,
  onCopied,
  onClose,
  onDetails,
}: {
  need: HomeNeed;
  block: string | null;
  busy: boolean;
  onOpenIssue: (id: string) => void;
  onDecide: (verb: "snooze" | "dismiss", secs?: number) => void;
  onCopied: () => void;
  onClose: () => void;
  /** Shows the technical fields (PR, head, reviewer, alias, command). */
  onDetails?: () => void;
}) {
  const item = MENU_ITEM;
  return (
    <>
      <button
        type="button"
        aria-hidden
        tabIndex={-1}
        className="fixed inset-0 z-20 cursor-default"
        onClick={onClose}
      />
      <div className="needmenu" role="menu" aria-label="more actions">
        {need.link && (
          <SafeLink className={`${item} block`} warnClassName={`${item} block`} href={need.link} role="menuitem">
            Open
          </SafeLink>
        )}
        {!need.link && need.issue && (
          <button className={item} role="menuitem" onClick={() => onOpenIssue(need.issue!)}>
            Open
          </button>
        )}
        <button
          className={item}
          role="menuitem"
          disabled={!!block || busy}
          title={block ?? undefined}
          onClick={() => onDecide("snooze", SNOOZE_24H)}
        >
          Snooze 24h
        </button>
        <button
          className={item}
          role="menuitem"
          disabled={!!block || busy}
          title={block ?? undefined}
          onClick={() => onDecide("snooze", SNOOZE_7D)}
        >
          Snooze 7d
        </button>
        <button
          className={`${item} text-fail`}
          role="menuitem"
          disabled={!!block || busy}
          title={block ?? undefined}
          onClick={() => onDecide("dismiss")}
        >
          Dismiss
        </button>
        {onDetails && (
          <button
            className={item}
            role="menuitem"
            onClick={() => {
              onDetails();
              onClose();
            }}
          >
            Details
          </button>
        )}
        <button
          className={item}
          role="menuitem"
          onClick={() => {
            copy(need.command).then(onCopied, () => undefined);
            onClose();
          }}
        >
          Copy command
        </button>
      </div>
    </>
  );
}
