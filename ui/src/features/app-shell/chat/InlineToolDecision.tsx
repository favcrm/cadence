import { isolate, type StagedWrite } from "../../decisions/stagedWrite";
import Button from "../../../ui/Button";

/** CAD-2755: the staged writes decided right in the chat card, with no drawer. */
export const isInlineTool = (write: StagedWrite): boolean =>
  write.kind === "custom_mcp_effect" || write.kind === "connected_effect";

export type InlineVerb = "approve" | "always" | "reject";

export interface InlineEntry {
  write: StagedWrite;
  busy: boolean;
  /** "Approved", "Always allowed" or "Rejected": the card shrinks to this. */
  done?: "Approved" | "Always allowed" | "Rejected";
  /** One line shown under the result or the buttons: a refusal or a failure. */
  note?: string;
}

function names(write: StagedWrite): { tool: string; connection: string } {
  if (write.customMcp) return { tool: write.customMcp.tool, connection: write.customMcp.connectionName };
  const c = write.connected;
  return { tool: c?.slug ?? write.title, connection: c?.preview?.toolkitLabel ?? c?.toolkit ?? "" };
}

/** The operation or the main input, when the staged write names one. */
function operationLine(write: StagedWrite): string | null {
  const preview = write.customMcp?.preview ?? write.connected?.preview;
  if (preview?.title) return preview.title;
  const first = write.changes[0];
  if (first === undefined || typeof first.after !== "string") return null;
  return `${first.label}: ${first.after}`;
}

export default function InlineToolDecision({
  entry,
  canAlwaysAllow,
  onDecide,
  t,
}: {
  entry: InlineEntry;
  canAlwaysAllow: boolean;
  onDecide: (verb: InlineVerb) => void;
  t: (key: string) => string;
}) {
  const { write, busy, done, note } = entry;
  const { tool, connection } = names(write);
  if (done !== undefined) {
    return (
      <article className="app-assistant-card" data-chat-staged data-inline-decided>
        <p className="m-0 text-label text-ink-300">
          {t(done)} · <bdi>{isolate(tool)}</bdi>
        </p>
        {note && <p className="m-0 text-label text-ink-400" role="status">{note}</p>}
      </article>
    );
  }
  const heading = t("Run {tool} on {connection}?")
    .replace("{tool}", () => `⁨${isolate(tool)}⁩`)
    .replace("{connection}", () => `⁨${isolate(connection)}⁩`);
  const operation = operationLine(write);
  return (
    <article className="app-assistant-card" data-chat-staged data-inline-decision>
      <div className="app-assistant-card-head">
        <strong className="text-label text-ink-100">{heading}</strong>
        <span className="text-micro text-ink-400">{t("Waiting")}</span>
      </div>
      {operation && <p className="m-0 text-label text-ink-300"><bdi>{isolate(operation)}</bdi></p>}
      <details>
        <summary className="text-label text-ink-300">{t("Show input")}</summary>
        <pre className="text-label text-ink-300" style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}>{isolate(write.raw)}</pre>
      </details>
      <div className="flex gap-2">
        <Button size="sm" variant="primary" disabled={busy} onClick={() => onDecide("approve")}>{t("Approve")}</Button>
        {canAlwaysAllow && write.kind === "custom_mcp_effect" && (
          <Button size="sm" variant="secondary" disabled={busy} onClick={() => onDecide("always")}>{t("Always allow")}</Button>
        )}
        <Button size="sm" variant="ghost" disabled={busy} onClick={() => onDecide("reject")}>{t("Reject")}</Button>
      </div>
      {note && <p className="m-0 text-label text-fail" role="alert">{note}</p>}
    </article>
  );
}
