import { useEffect, useId, useRef, useState } from "react";
import type React from "react";
import Button from "../../../ui/Button";
import { useTranslate } from "../../../lib/localeText";

/** A modal confirm: the operator's typed `expect` must equal
 *  `match` before `confirmLabel` enables. Esc cancels, the input or the
 *  dialog takes focus on open, Tab stays inside, and focus returns to the
 *  opener on close. */
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
  const t = useTranslate();
  const [typed, setTyped] = useState("");
  const inputId = useId();
  const inputRef = useRef<HTMLInputElement | null>(null);
  const dialogRef = useRef<HTMLElement | null>(null);
  const openerRef = useRef<HTMLElement | null>(null);
  const gatedInput = useRef(expect !== undefined);
  gatedInput.current = expect !== undefined;
  const gated = expect !== undefined ? typed.trim() === (match ?? "") : true;

  useEffect(() => {
    const active = document.activeElement;
    openerRef.current = active instanceof HTMLElement ? active : null;
    (gatedInput.current ? inputRef.current : dialogRef.current)?.focus();
    return () => {
      const opener = openerRef.current;
      if (opener?.isConnected) opener.focus();
    };
  }, []);

  const focusable = (): HTMLElement[] => {
    const root = dialogRef.current;
    if (!root) return [];
    return Array.from(
      root.querySelectorAll<HTMLElement>(
        'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])',
      ),
    ).filter((el) => el.getAttribute("aria-hidden") !== "true");
  };

  const onKeyDown = (event: React.KeyboardEvent<HTMLElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      onCancel();
      return;
    }
    if (event.key !== "Tab") return;
    const items = focusable();
    if (items.length === 0) {
      event.preventDefault();
      dialogRef.current?.focus();
      return;
    }
    const first = items[0]!;
    const last = items[items.length - 1]!;
    const active = document.activeElement;
    if (event.shiftKey) {
      if (active === first || active === dialogRef.current) {
        event.preventDefault();
        last.focus();
      }
    } else if (active === last || active === dialogRef.current) {
      event.preventDefault();
      first.focus();
    }
  };

  return (
    <div className="crm-confirm-wrap" role="presentation">
      <div className="crm-confirm-scrim" onClick={onCancel} />
      <section
        ref={dialogRef}
        tabIndex={-1}
        className="card crm-confirm grid gap-3"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onKeyDown={onKeyDown}
      >
        <h4 className="text-cardtitle font-medium text-ink-100">{title}</h4>
        <div className="text-label text-ink-300">{body}</div>
        {expect !== undefined && (
          <div className="crm-field">
            <label className="text-label text-ink-300" htmlFor={inputId}>
              {inputLabel ?? `${t("Type")} ${match} ${t("to confirm")}`}
            </label>
            <input
              id={inputId}
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
            {t("Cancel")}
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
