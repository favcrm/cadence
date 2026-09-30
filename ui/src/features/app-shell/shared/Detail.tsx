import type { ReactNode } from "react";

/**
 * Shared description-list detail grid (CAD-866): the `<dl>` of
 * label/value pairs that the CRM customer drawer and the app-views/v1
 * detail view both hand-rolled (`crm-detail` / `av-detail`).
 *
 * Semantics: a real `<dl>` with `<dt>` terms and `<dd>` values so a
 * screen reader announces the label/value relationship; long values
 * wrap via `overflow-wrap: anywhere` from the shared `.sdetail` CSS.
 * The two-column term/value grid collapses naturally as values wrap —
 * no truncation, no hidden content.
 */

export interface DetailItem {
  /** Stable React key (a field id). */
  key: string;
  /** The `<dt>` term — a plain label. */
  term: ReactNode;
  /** The `<dd>` value — text, a chip, or formatted content. */
  value: ReactNode;
}

export default function Detail({
  items,
  label,
  className,
}: {
  items: DetailItem[];
  /** Optional `aria-label` naming the list (e.g. "Customer fields"). */
  label?: string;
  /** Extra class on the `<dl>` (e.g. a consumer's `crm-detail`). */
  className?: string;
}) {
  return (
    <dl
      className={["sdetail", className ?? ""].filter(Boolean).join(" ")}
      aria-label={label}
    >
      {items.map((item) => (
        <div key={item.key}>
          <dt>{item.term}</dt>
          <dd>{item.value}</dd>
        </div>
      ))}
    </dl>
  );
}
