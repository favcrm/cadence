import { newBlock, type EditorBlock } from "./useEmailDraft";

const MAX_BLOCKS = 12;
const KINDS: [EditorBlock["kind"], string][] = [
  ["heading", "Heading"],
  ["paragraph", "Paragraph"],
  ["button", "Button"],
];

/**
 * Visual mode (CAD-1057): the email body as editable blocks, styled
 * like the rendered email. Text is edited in place; blocks can be
 * added, moved and removed. Nothing here is rendered as the email —
 * the host render below it is what gets approved.
 */
export default function EmailBlocksCanvas({
  blocks,
  disabled,
  onChange,
}: {
  blocks: EditorBlock[];
  disabled: boolean;
  onChange: (blocks: EditorBlock[]) => void;
}) {
  const update = (key: number, patch: Partial<EditorBlock>) =>
    onChange(blocks.map((block) => (block.key === key ? { ...block, ...patch } : block)));
  const move = (index: number, by: -1 | 1) => {
    const next = blocks.slice();
    const [item] = next.splice(index, 1);
    next.splice(index + by, 0, item!);
    onChange(next);
  };

  return (
    <div className="crm-canvas" aria-label="Email body blocks">
      {blocks.map((block, index) => (
        <div key={block.key} className="crm-block" data-block={block.kind}>
          <div className="crm-block-body">
            {block.kind === "heading" && (
              <input
                className="crm-block-heading"
                aria-label={`Heading, block ${index + 1}`}
                value={block.text}
                maxLength={120}
                disabled={disabled}
                placeholder="Heading"
                onChange={(e) => update(block.key, { text: e.target.value })}
              />
            )}
            {block.kind === "paragraph" && (
              <textarea
                className="crm-block-paragraph"
                aria-label={`Paragraph, block ${index + 1}`}
                rows={3}
                value={block.text}
                maxLength={2000}
                disabled={disabled}
                placeholder="Write your message"
                onChange={(e) => update(block.key, { text: e.target.value })}
              />
            )}
            {block.kind === "button" && (
              <span className="crm-block-button">
                <input
                  aria-label={`Button label, block ${index + 1}`}
                  value={block.label}
                  maxLength={60}
                  disabled={disabled}
                  placeholder="Button label"
                  onChange={(e) => update(block.key, { label: e.target.value })}
                />
                <input
                  aria-label={`Button link, block ${index + 1}`}
                  className="num"
                  value={block.url}
                  disabled={disabled}
                  placeholder="https://"
                  onChange={(e) => update(block.key, { url: e.target.value })}
                />
              </span>
            )}
          </div>
          <span className="crm-block-tools">
            <button type="button" className="lnk" aria-label={`Move block ${index + 1} up`} disabled={disabled || index === 0} onClick={() => move(index, -1)}>
              ↑
            </button>
            <button type="button" className="lnk" aria-label={`Move block ${index + 1} down`} disabled={disabled || index === blocks.length - 1} onClick={() => move(index, 1)}>
              ↓
            </button>
            <button type="button" className="lnk" aria-label={`Remove block ${index + 1}`} disabled={disabled || blocks.length === 1} onClick={() => onChange(blocks.filter((b) => b.key !== block.key))}>
              ✕
            </button>
          </span>
        </div>
      ))}
      <div className="crm-addblock" role="group" aria-label="Add block">
        <span className="text-label text-ink-400">+ Add block</span>
        {KINDS.map(([kind, label]) => (
          <button
            key={kind}
            type="button"
            className="btn btn-sm"
            disabled={disabled || blocks.length >= MAX_BLOCKS}
            onClick={() => onChange([...blocks, newBlock(kind)])}
          >
            {label}
          </button>
        ))}
      </div>
    </div>
  );
}
