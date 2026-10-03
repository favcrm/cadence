import type { ReactNode } from "react";
import ChatCsvImport from "../ChatCsvImport";
import type { HostScope } from "../hostActions";
import type { HostAttachmentId } from "./registry";

/**
 * The host capability registry for attachments (CAD-1109). An attachment is a
 * host-implemented composer extension an app opts into BY ID; listing one
 * grants no authority, and the capability's own server calls keep their own
 * proof. The host renders it only when the pane has a route scope and the
 * viewer is the operator and not read-only (the caller checks both).
 */
export interface CapabilityContext {
  scope: HostScope;
  canWrite: boolean;
  /** Sends the scoped chat message carrying a capability's directive. */
  sendIntent: (intent: { cadence_csv_import: { request_id: string; confirm_token: string } }) => Promise<string | null>;
}

export function renderCapability(id: HostAttachmentId, label: string | null, ctx: CapabilityContext): ReactNode {
  switch (id) {
    case "csv-import":
      return (
        <details className="app-chat-import" data-chat-attachment={id}>
          <summary className="text-label text-ink-300">{label ?? "Import a customer list"}</summary>
          <ChatCsvImport
            // CAD-1016: a context change remounts the import, so a pending
            // plan, preview or choices from the prior scope never bleed over.
            key={`${ctx.scope.installId}:${ctx.scope.contextId}`}
            scope={ctx.scope}
            canWrite={ctx.canWrite}
            onSendIntent={ctx.sendIntent}
          />
        </details>
      );
  }
}
