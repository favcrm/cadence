import { useRef } from "react";
import type { DirectiveCardSpec } from "./contract";
import { runHostAction, type ActionContext } from "./actions";
import { hideIds } from "./directive";

/**
 * A declared text card (CAD-1109, D3). Every string is React text: no
 * markdown, no anchor, no style or class from the descriptor or the payload.
 * The buttons are the descriptor's alone; a click runs the host action
 * registry entry with the descriptor's parameters, never the payload's.
 */
export default function DirectiveCard({
  card,
  fields,
  actions,
}: {
  card: DirectiveCardSpec;
  fields: { label: string; value: string }[];
  actions: ActionContext | null;
}) {
  const last = useRef(0);
  return (
    <div className="card app-chat-card" data-chat-card="directive">
      <div className="text-label font-medium text-ink-100">{card.title}</div>
      {card.text !== null && <div className="text-micro text-ink-400">{card.text}</div>}
      {fields.length > 0 && (
        <dl className="text-micro text-ink-300">
          {fields.map((f, i) => (
            <div key={i} className="flex gap-1">
              <dt className="text-ink-500">{f.label}:</dt>
              <dd className="min-w-0 break-words">{hideIds(f.value)}</dd>
            </div>
          ))}
        </dl>
      )}
      {actions !== null && card.buttons.length > 0 && (
        <div className="flex gap-2 mt-1">
          {card.buttons.map((b, i) => (
            <button
              key={i}
              type="button"
              className="btn btn-secondary btn-sm"
              data-chat-card-button={b.run}
              onClick={() => {
                // A double click is one navigation.
                const now = Date.now();
                if (now - last.current < 400) return;
                last.current = now;
                runHostAction(b.run, { view: b.view }, actions);
              }}
            >
              {b.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

/** Guard 10: a token-bearing message with no declared directive. */
export function ConfirmationCard() {
  return (
    <div className="card app-chat-card" data-chat-card="directive">
      <div className="text-label font-medium text-ink-100">Confirmation sent</div>
      <div className="text-micro text-ink-400">The assistant has your confirmation.</div>
    </div>
  );
}
