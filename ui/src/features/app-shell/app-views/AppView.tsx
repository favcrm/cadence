import { useId } from "react";
import {
  cellText,
  type AppViewCell,
  type AppViewDescriptor,
  type AppViewField,
  type AppViewRow,
  type AppViewView,
} from "./contract";

/**
 * The one shared renderer for an app-views/v1 descriptor (CAD-861,
 * toward CAD-811). Both worked examples — CRM and Social Content —
 * render through this component, and any future package descriptor
 * would too.
 *
 * The contract's security model lives here structurally, not by
 * sanitization: every package-supplied string is a React text child.
 * There is no dangerouslySetInnerHTML, no eval/new Function, no
 * dynamic import, no href/src built from descriptor data, and no
 * element whose tag comes from the data. An `<script>`-looking cell
 * renders as inert visible text; a descriptor that tries to carry
 * `html`/`href`/`actor`/`install_id` keys never reaches this
 * component because parseAppView refuses it first.
 *
 * Mutations stay the host's: form views render as a disabled preview
 * of the create form the host may later wire, never as a live submit.
 */

/** Descriptor views plus the caller's own fixture rows, keyed by
 *  view id. Rows are *not* descriptor content — the host picks them
 *  (synthetic today, live records later) and validates them through
 *  `fixtureRows` before render. */
export interface AppViewData {
  descriptor: AppViewDescriptor;
  rows: Record<string, AppViewRow[]>;
  /** Which view to show first. Defaults to the first declared view. */
  initialViewId?: string;
}

export default function AppView({ descriptor, rows, initialViewId }: AppViewData) {
  const active =
    descriptor.views.find((v) => v.id === initialViewId) ?? descriptor.views[0];
  // This renderer is a preview surface today: views are rendered one
  // at a time, picked by the caller. Inter-view navigation stays a
  // host concern so a descriptor cannot name routes.
  return <ViewBody view={active} descriptor={descriptor} rows={rows[active.id] ?? []} />;
}

function ViewBody({
  view,
  descriptor,
  rows,
}: {
  view: AppViewView;
  descriptor: AppViewDescriptor;
  rows: AppViewRow[];
}) {
  const titleId = useId();
  return (
    <section className="av" aria-labelledby={titleId} data-app-view={view.id}>
      <header className="av-head">
        <h3 id={titleId} className="text-cardtitle font-medium text-ink-100" tabIndex={-1}>
          {view.title}
        </h3>
        <p className="num text-micro text-ink-500">
          {descriptor.app} · {view.kind} view · {descriptor.title}
        </p>
      </header>
      {view.kind === "table" && <TableView view={view} rows={rows} />}
      {view.kind === "detail" && <DetailView view={view} rows={rows} />}
      {view.kind === "form" && <FormView view={view} />}
    </section>
  );
}

function fieldMap(view: AppViewView): Map<string, AppViewField> {
  return new Map((view.fields ?? view.previewOf ?? []).map((f) => [f.id, f]));
}

function Cell({ field, value }: { field: AppViewField; value: AppViewCell | undefined }) {
  const text = cellText(value);
  if (field.format === "enum") {
    return <span className="chip">{text}</span>;
  }
  if (field.format === "tags") {
    const items = Array.isArray(value) ? value : text === "—" ? [] : [text];
    return items.length === 0 ? (
      <span className="text-ink-500">—</span>
    ) : (
      <span className="av-tags">
        {items.map((item) => (
          <span key={item} className="chip">
            {item}
          </span>
        ))}
      </span>
    );
  }
  if (field.format === "number" || field.format === "date" || field.format === "datetime") {
    return <span className="num">{text}</span>;
  }
  return <>{text}</>;
}

function TableView({ view, rows }: { view: AppViewView; rows: AppViewRow[] }) {
  const fields = fieldMap(view);
  const columns = view.columns ?? [];
  return rows.length === 0 ? (
    <p className="card px-4 py-5 text-secondary text-ink-400" role="status">
      No fixture rows declared for this view — the preview stays empty rather than inventing data.
    </p>
  ) : (
    <div className="av-table-wrap" tabIndex={0} role="region" aria-label={`${view.title} rows`}>
      <table className="av-table">
        <thead>
          <tr>
            {columns.map((c) => {
              const field = fields.get(c.field)!;
              return (
                <th key={c.field} scope="col">
                  {c.label ?? field.label}
                </th>
              );
            })}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, i) => (
            <tr key={i}>
              {columns.map((c) => {
                const field = fields.get(c.field)!;
                return (
                  <td key={c.field}>
                    <Cell field={field} value={row[c.field]} />
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function DetailView({ view, rows }: { view: AppViewView; rows: AppViewRow[] }) {
  const fields = view.fields ?? [];
  const row = rows[0] ?? {};
  return (
    <dl className="av-detail" aria-label={`${view.title} fields`}>
      {fields.map((field) => (
        <div key={field.id}>
          <dt>{field.label}</dt>
          <dd>
            <Cell field={field} value={row[field.id]} />
          </dd>
        </div>
      ))}
    </dl>
  );
}

/**
 * A form view renders the host's *future* create form as a labelled,
 * disabled preview: real label/id association and focus-visible
 * styling, but no submit target, no handler, and no descriptor data
 * ever reaching an action. The disabled state is the honest truth —
 * this increment wires no live mutation.
 */
function FormView({ view }: { view: AppViewView }) {
  const fields = view.previewOf ?? [];
  const uid = useId();
  return (
    <form
      className="av-form"
      aria-label={`${view.title} preview`}
      onSubmit={(e) => e.preventDefault()}
    >
      {fields.map((field) => {
        const id = `${uid}-${field.id}`;
        return (
          <div key={field.id} className="av-field">
            <label className="text-label text-ink-300" htmlFor={id}>
              {field.label}
            </label>
            {field.format === "enum" ? (
              <select id={id} className="av-input" disabled aria-label={field.label}>
                <option value="">{field.values?.join(" / ") ?? "—"}</option>
              </select>
            ) : field.kind === "list" || field.format === "tags" ? (
              <textarea
                id={id}
                className="av-input"
                rows={2}
                disabled
                aria-label={field.label}
                placeholder="Comma-separated values"
              />
            ) : (
              <input
                id={id}
                className="av-input"
                disabled
                aria-label={field.label}
                autoComplete="off"
                inputMode={field.format === "number" ? "decimal" : undefined}
              />
            )}
          </div>
        );
      })}
      <div className="av-form-foot">
        <button
          type="submit"
          className="btn btn-primary"
          disabled
          title="Disabled preview — saving stays with the host's own screens"
        >
          Save (preview)
        </button>
        <p className="text-micro text-ink-500">
          Disabled preview of the host's create form. No data leaves the browser.
        </p>
      </div>
    </form>
  );
}
