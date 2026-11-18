import "./controls.css";

/**
 * CAD-2438: the board's on/off switch (`button role="switch"`), shared by
 * Manage app and the agent Skills tab. `disabled` is the native attribute, for
 * a switch that cannot be used. `pending` keeps the button focusable while a
 * save runs (`aria-disabled`, clicks ignored), so focus is not dropped.
 */
export default function Switch({
  checked,
  onToggle,
  label,
  disabled = false,
  pending = false,
  id,
}: {
  checked: boolean;
  onToggle: () => void;
  /** The accessible name; a switch has no visible text of its own. */
  label: string;
  disabled?: boolean;
  pending?: boolean;
  id?: string;
}) {
  return (
    <button
      id={id}
      type="button"
      role="switch"
      className="switch"
      aria-checked={checked}
      aria-label={label}
      aria-disabled={pending || undefined}
      disabled={disabled}
      onClick={() => {
        if (!pending) onToggle();
      }}
    />
  );
}
