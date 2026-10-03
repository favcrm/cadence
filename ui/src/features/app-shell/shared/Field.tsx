import { useId, type ReactNode } from "react";

/**
 * Shared labelled form field (CAD-866). One visible `<label htmlFor>`
 * bound to a single control, plus optional hint and error text wired
 * with `aria-describedby` and `aria-invalid` — so the accessible
 * label/hint/error relationship is built once, not re-typed per screen.
 *
 * The caller owns the control and must spread the provided
 * `FieldControlProps` onto it (`children={(c) => <input {...c} … />}`),
 * so the `id`, described-by and invalid markers reach the real element —
 * including composite controls like `Select` that forward their own
 * `id`. There is no prop-injection/`cloneElement` path: the render prop
 * is the only API, which keeps precedence obvious — whatever the caller
 * renders is exactly what the label points at.
 *
 * Accessibility contract (modern-web-guidance: forms):
 *  - A real `<label htmlFor>` — never a bare `<span>` or a placeholder.
 *  - `hint` links via `aria-describedby`; `error` sets `aria-invalid`
 *    on the control and renders `role="alert"` (assertive live region).
 *    When the caller sets `error` is its own timing decision — the
 *    component only guarantees that *whenever* an error is present it is
 *    announced; it does not decide to announce only on submit.
 *  - `required`/`disabled`/`readOnly` are exposed to the caller's
 *    control through `FieldControlProps` (and shown on the label), so an
 *    inert or mandatory state is real semantics, not styling alone.
 *
 * Styling stays canonical: the control keeps the board's `.field`
 * tokens. The wrapper carries only the generic `sfield` grid class;
 * a consumer that needs a legacy layout hook (e.g. CRM's `crm-field`)
 * passes it through `className`.
 */

/** Props the caller spreads onto its control element. */
export interface FieldControlProps {
  id: string;
  "aria-describedby"?: string;
  "aria-invalid"?: boolean;
  required?: boolean;
  disabled?: boolean;
  readOnly?: boolean;
}

export interface FieldProps {
  /** Visible label text — required; a placeholder is not a label. */
  label: ReactNode;
  /** Renders the control; must spread the supplied props onto it. */
  children: (props: FieldControlProps) => ReactNode;
  /** Optional explicit control id; a stable generated id is used otherwise. */
  id?: string;
  /** Supporting text under the label, linked by `aria-describedby`. */
  hint?: ReactNode;
  /** Validation/refusal message under the control; `role="alert"`. */
  error?: ReactNode;
  /** Marks the label and forwards `required` to the control. */
  required?: boolean;
  /** Forwards `disabled` to the control and marks the wrapper inert. */
  disabled?: boolean;
  /** Forwards `readOnly` to the control and marks the wrapper read-only. */
  readOnly?: boolean;
  /** Extra wrapper class — e.g. a consumer's legacy layout hook. */
  className?: string;
}

export default function Field(props: FieldProps) {
  const { label, children, id, hint, error, required, disabled, readOnly, className } = props;
  const generated = useId();
  const controlId = id ?? generated;
  const hasError = error !== undefined && error !== null && error !== "";
  const hintId = hint !== undefined && hint !== null ? `${controlId}-hint` : undefined;
  const errorId = hasError ? `${controlId}-error` : undefined;
  const describedBy = [hintId, errorId].filter(Boolean).join(" ") || undefined;

  const control = children({
    id: controlId,
    "aria-describedby": describedBy,
    "aria-invalid": hasError ? true : undefined,
    required: required || undefined,
    disabled: disabled || undefined,
    readOnly: readOnly || undefined,
  });

  return (
    <div
      className={["sfield", className ?? ""].filter(Boolean).join(" ")}
      data-state={disabled ? "disabled" : readOnly ? "read-only" : undefined}
    >
      <label className="sfield-label text-label text-ink-300" htmlFor={controlId}>
        {label}
        {required ? (
          <span className="sfield-req" aria-hidden="true">
            {" "}
            *
          </span>
        ) : null}
      </label>
      {hint !== undefined && hint !== null ? (
        <p className="sfield-hint text-micro text-ink-500" id={hintId}>
          {hint}
        </p>
      ) : null}
      {control}
      {hasError ? (
        <p className="sfield-error text-label text-fail" id={errorId} role="alert">
          {error}
        </p>
      ) : null}
    </div>
  );
}
