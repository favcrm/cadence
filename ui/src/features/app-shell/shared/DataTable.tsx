import type { ReactNode } from "react";

/**
 * Shared semantic data table (CAD-866): the `<table>`-in-a-scrollable-
 * region shape that the CRM record list and the app-views/v1 table
 * preview both hand-rolled.
 *
 * The contract is accessibility + overflow, not styling novelty:
 *  - Real `<thead>`/`<th scope="col">` column headers, `<tbody>` rows.
 *  - The wrapper is a focusable `role="region"` (`tabIndex={0}`) so a
 *    keyboard user can scroll a table that overflows at narrow widths;
 *    its `aria-label` tells them why the region exists.
 *  - `overflow-wrap: anywhere` + a `min-width` floor come from the
 *    shared `.stable`/`.stable-wrap` CSS, so long tokens wrap and the
 *    *wrapper* scrolls — the document never grows past the viewport.
 *
 * Sorting, search, selection and paging stay with the caller: this
 * component renders rows and columns; the semantics of a click or a
 * committed query are the consumer's, unchanged.
 */

export interface DataTableColumn<Row> {
  /** Stable key (field id). Used for React keys, never rendered raw. */
  key: string;
  /** Column header content. */
  header: ReactNode;
  /** Header content is a plain string for `scope="col"` labelling only. */
  cell: (row: Row, index: number) => ReactNode;
  /** Extra class on every `td` of this column. */
  cellClassName?: string;
}

export interface DataTableProps<Row> {
  columns: DataTableColumn<Row>[];
  rows: Row[];
  /** Stable React key for a row (record id, row number). */
  rowKey: (row: Row, index: number) => string | number;
  /** Accessible name for the scrollable region (e.g. "Customers table"). */
  label: string;
  /** Extra class on the scroll wrapper (e.g. a consumer's `crm-table-wrap`). */
  wrapClassName?: string;
  /** Extra class on the `<table>` (e.g. `crm-table` for its width floor). */
  tableClassName?: string;
  /** Optional per-row attributes (e.g. a `data-*` marker). */
  rowProps?: (row: Row, index: number) => Record<string, unknown>;
}

export default function DataTable<Row>({
  columns,
  rows,
  rowKey,
  label,
  wrapClassName,
  tableClassName,
  rowProps,
}: DataTableProps<Row>) {
  return (
    <div
      className={["stable-wrap", wrapClassName ?? ""].filter(Boolean).join(" ")}
      tabIndex={0}
      role="region"
      aria-label={label}
    >
      <table className={["stable", tableClassName ?? ""].filter(Boolean).join(" ")}>
        <thead>
          <tr>
            {columns.map((column) => (
              <th key={column.key} scope="col">
                {column.header}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, index) => (
            <tr key={rowKey(row, index)} {...(rowProps?.(row, index) ?? {})}>
              {columns.map((column) => (
                <td key={column.key} className={column.cellClassName}>
                  {column.cell(row, index)}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
