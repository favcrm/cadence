/**
 * CAD-2543: a record view is a canvas tab that reads one stored `detail`
 * record card (an email) in full. The tab holds the card's own content, laid
 * out from the thread entry already in hand: there is no request, and every
 * value is plain text.
 */
import type { RecordDetail } from "../home/chatCards";

export interface RecordViewRef {
  /** The thread entry's key (`e<seq>`): one tab per card. */
  key: string;
  id: string;
  title: string;
  detail: RecordDetail;
}

/** The `record:` prefix keeps it clear of artifact ids (`art-…`) and app views (`app:…`). */
export const recordViewId = (key: string): string => `record:${key}`;
