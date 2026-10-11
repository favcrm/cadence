import { useEffect, useRef } from "react";
import { useLocale } from "../../../lib/locale";
import { newBlock, type EditorBlock } from "./useEmailDraft";

export const MAX_BLOCKS = 12;

/**
 * Direct-visual email canvas (CAD-1146, approved charcoal/teal
 * reference): the operator edits supported heading/paragraph/button
 * content directly on the white email, not in a separate panel. Each
 * block is host-rendered plain text — React text nodes only, never
 * `dangerouslySetInnerHTML`, never an iframe, never user HTML — so no
 * user markup executes and the sandbox is untouched. Editing uses
 * `plaintext-only` (paste is forced to plain text where the browser
 * lacks it) and the existing grammar checks still refuse markup at
 * save time; the host render below this canvas stays the approved
 * truth. The locked unsubscribe footer lives outside this component.
 */
export default function EmailBlocksCanvas({
  blocks,
  disabled,
  brandName,
  device,
  canUndo,
  onUndo,
  onStructure,
  onText,
}: {
  blocks: EditorBlock[];
  disabled: boolean;
  /** Sender brand line shown atop the email (preview-only label). */
  brandName: string;
  device: "desktop" | "mobile";
  canUndo: boolean;
  onUndo: () => void;
  /** Structural change (add/move/delete): recorded for undo. */
  onStructure: (blocks: EditorBlock[]) => void;
  /** Keystroke text edit: never touches undo history. */
  onText: (key: number, patch: Partial<EditorBlock>) => void;
}) {
  const { t, formatNumber } = useLocale();
  const move = (index: number, by: -1 | 1) => {
    const next = blocks.slice();
    const [item] = next.splice(index, 1);
    if (item === undefined) return;
    next.splice(index + by, 0, item);
    onStructure(next);
  };

  return (
    <div className="crm-vcanvas">
      <article className="crm-vemail" data-device={device} aria-label={t("Email canvas — edit directly")}>
        <div className="crm-vbrand">{brandName}</div>
        <div className="crm-vbody">
          {blocks.map((block, index) => (
            <div key={block.key} className="crm-vblock" data-block={block.kind}>
              {block.kind === "heading" && (
                <VisualText
                  kind="heading"
                  className="crm-vedit crm-vheading"
                  label={`${t("Email heading")}, ${t("block")} ${formatNumber(index + 1)}`}
                  value={block.text}
                  placeholder={t("Heading")}
                  multiline={false}
                  disabled={disabled}
                  onInput={(text) => onText(block.key, { text })}
                />
              )}
              {block.kind === "paragraph" && (
                <VisualText
                  kind="paragraph"
                  className="crm-vedit crm-vparagraph"
                  label={`${t("Email paragraph")}, ${t("block")} ${formatNumber(index + 1)}`}
                  value={block.text}
                  placeholder={t("Write your message")}
                  multiline
                  disabled={disabled}
                  onInput={(text) => onText(block.key, { text })}
                />
              )}
              {block.kind === "button" && (
                <span className="crm-vbutton-wrap">
                  <VisualText
                    kind="label"
                    className="crm-vedit crm-vcta"
                    label={`${t("Button label")}, ${t("block")} ${formatNumber(index + 1)}`}
                    value={block.label}
                    placeholder={t("Button label")}
                    multiline={false}
                    disabled={disabled}
                    onInput={(label) => onText(block.key, { label })}
                  />
                  <input
                    className="crm-vurl"
                    aria-label={`${t("Button link")}, ${t("block")} ${formatNumber(index + 1)}`}
                    value={block.url}
                    disabled={disabled}
                    placeholder="https://"
                    autoComplete="off"
                    spellCheck={false}
                    onChange={(e) => onText(block.key, { url: e.target.value })}
                  />
                </span>
              )}
              <span className="crm-vtools" role="group" aria-label={`${t("Block")} ${formatNumber(index + 1)} ${t("actions")}`}>
                <button
                  type="button"
                  aria-label={`${t("Move block")} ${formatNumber(index + 1)} ${t("up")}`}
                  disabled={disabled || index === 0}
                  onClick={() => move(index, -1)}
                >
                  ↑
                </button>
                <button
                  type="button"
                  aria-label={`${t("Move block")} ${formatNumber(index + 1)} ${t("down")}`}
                  disabled={disabled || index === blocks.length - 1}
                  onClick={() => move(index, 1)}
                >
                  ↓
                </button>
                <button
                  type="button"
                  aria-label={`${t("Remove block")} ${formatNumber(index + 1)}`}
                  disabled={disabled}
                  onClick={() => onStructure(blocks.filter((b) => b.key !== block.key))}
                >
                  ✕
                </button>
              </span>
            </div>
          ))}
          {blocks.length === 0 && (
            <p className="crm-vempty">{t("Start with text, a heading, or your own HTML.")}</p>
          )}
        </div>
      </article>
      <div className="crm-vfoot">
        {canUndo && (
          <button type="button" className="lnk" onClick={onUndo} disabled={disabled}>
            {t("Undo last block change")}
          </button>
        )}
        <span className="crm-vhint" data-canvas-hint>
          {t("Click to edit · ⋯ for block actions · The unsubscribe footer is always included.")}
        </span>
      </div>
    </div>
  );
}

/** Add-block tools for the Email tab toolbar (visual mode only). */
export function AddBlockTools({
  disabled,
  full,
  onAdd,
}: {
  disabled: boolean;
  full: boolean;
  onAdd: (block: EditorBlock) => void;
}) {
  const { t } = useLocale();
  if (!full) return null;
  return (
    <span className="crm-addtools" role="group" aria-label={t("Add block")}>
      <button type="button" disabled={disabled} onClick={() => onAdd(newBlock("paragraph"))}>
        + {t("Text")}
      </button>
      <button type="button" disabled={disabled} onClick={() => onAdd(newBlock("heading"))}>
        {t("Heading")}
      </button>
      <button type="button" disabled={disabled} onClick={() => onAdd(newBlock("button"))}>
        {t("Button")}
      </button>
    </span>
  );
}

/**
 * One plain-text editable run. Uncontrolled while focused (React only
 * pushes external values — undo, move, add — so the caret never jumps
 * mid-keystroke); every input reports plain text upward.
 */
function VisualText({
  kind,
  className,
  label,
  value,
  placeholder,
  multiline,
  disabled,
  onInput,
}: {
  kind: "heading" | "paragraph" | "label";
  className: string;
  label: string;
  value: string;
  placeholder: string;
  multiline: boolean;
  disabled: boolean;
  onInput: (text: string) => void;
}) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = ref.current;
    if (el !== null && el.textContent !== value) el.textContent = value;
  }, [value]);

  return (
    <div
      ref={ref}
      className={className}
      role={kind === "heading" ? "heading" : "textbox"}
      aria-level={kind === "heading" ? 2 : undefined}
      aria-label={label}
      aria-multiline={kind === "heading" ? undefined : multiline}
      data-visual-text={label}
      data-placeholder={value === "" ? placeholder : undefined}
      contentEditable={disabled ? undefined : "plaintext-only"}
      suppressContentEditableWarning
      spellCheck
      onInput={(e) => {
        let text = e.currentTarget.textContent ?? "";
        if (!multiline && text.includes("\n")) {
          text = text.replace(/\n+/g, " ");
          e.currentTarget.textContent = text;
        }
        onInput(text);
      }}
      // Where `plaintext-only` is unsupported the browser could offer
      // rich paste: force plain text so no markup ever enters the draft.
      onPaste={(e) => {
        e.preventDefault();
        const text = e.clipboardData?.getData("text/plain") ?? "";
        if (typeof document !== "undefined" && "execCommand" in document) {
          try {
            if (document.execCommand("insertText", false, text)) return;
          } catch {
            // Fall through to the manual insert below.
          }
        }
        const el = e.currentTarget;
        el.textContent = `${el.textContent ?? ""}${text}`;
        onInput(el.textContent ?? "");
      }}
      onKeyDown={
        multiline
          ? undefined
          : (e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                e.currentTarget.blur();
              }
            }
      }
    />
  );
}
