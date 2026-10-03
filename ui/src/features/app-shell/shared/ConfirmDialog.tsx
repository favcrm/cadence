import { useEffect, useRef, useState } from "react";
import type React from "react";
import Button from "../../../ui/Button";

/** A modal confirm: the operator's typed `expect` must equal
 *  `match` before `confirmLabel` enables. Esc cancels, the input
 *  autofocuses and a submit re-arms only through a fresh open. */
export default function ConfirmDialog({
  title,
  body,
  expect,
  match,
  inputLabel,
  confirmLabel,
  pending,
  error,
  onConfirm,
  onCancel,
}: {
  title: string;
  body: React.ReactNode;
  /** When set, the input gate: confirm enables only when the typed
   *  text equals `match` exactly. */
  expect?: string;
  match?: string;
  inputLabel?: string;
  confirmLabel: string;
  pending: boolean;
  error: string | null;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const [typed, setTyped] = useState("");
  const inputRef = useRef<HTMLInputElement | null>(null);
  useEffect(() => {
    if (expect !== undefined) inputRef.current?.focus();
  }, [expect]);
  const gated = expect !== undefined ? typed.trim() === (match ?? "") : true;
  return (
    <div className="crm-confirm-wrap" role="presentation">
      <div className="crm-confirm-scrim" onClick={onCancel} />
      <section
        className="card crm-confirm grid gap-3"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.stopPropagation();
            onCancel();
          }
        }}
      >
        <h4 className="text-cardtitle font-medium text-ink-100">{title}</h4>
        <div className="text-label text-ink-300">{body}</div>
        {expect !== undefined && (
          <div className="crm-field">
            <label className="text-label text-ink-300" htmlFor="crm-confirm-input">
              {inputLabel ?? `Type ${match} to confirm`}
            </label>
            <input
              id="crm-confirm-input"
              ref={inputRef}
              className="field num"
              inputMode="numeric"
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
              maxLength={12}
              autoComplete="off"
              disabled={pending}
            />
          </div>
        )}
        {error && (
          <p className="text-label text-fail" role="alert">
            {error}
          </p>
        )}
        <div className="crm-toolbar">
          <Button size="sm" onClick={onCancel} disabled={pending}>
            Cancel
          </Button>
          <Button
            size="sm"
            variant="danger"
            loading={pending}
            disabled={pending || !gated}
            onClick={onConfirm}
          >
            {confirmLabel}
          </Button>
        </div>
      </section>
    </div>
  );
}
