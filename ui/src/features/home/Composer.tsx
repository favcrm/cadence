import type { ThreadRef } from "../../lib/types";
import SharedComposer from "../app-shell/chat/Composer";
import type { AttachDestination } from "../app-shell/chat/attach";
import { sendToMaster } from "./send";
import { newMessageId } from "./thread";
import { useAssistantComposerLabels } from "../assistant/AssistantIdentity";

/**
 * Home's composer is the shared one (CAD-1168) at full density: the
 * draft owns its render (typing never re-renders the thread above), an
 * Ask-master `seed` prefill carries its cited rows as `refs` chips, and
 * the CAD-551 slash menu completes verbs — a `/verb [arg]` submits
 * through `onCommand`, never to the thread.
 *
 * CAD-600: this is the floating dock — pinned to the panel's bottom by
 * its wrapper, the textarea growing with the draft up to ~8 lines and
 * scrolling past that (the CSS max-height), and the focus staying in
 * the box after a send.
 */
export default function Composer({
  block,
  seed,
  onCommand,
  onSent,
  attachEnabled = true,
  attachDestination,
  attachReason = "A writable Home conversation is required to attach files.",
}: {
  block: string | null;
  seed: { text: string; n: number; refs?: ThreadRef[] };
  onCommand: (name: string, arg: string) => void;
  /** Enter or the button actually submitted — chat, slash, or an Ask-master draft. */
  onSent: () => void;
  attachEnabled?: boolean;
  attachDestination?: AttachDestination;
  attachReason?: string;
}) {
  const { placeholder, ariaLabel } = useAssistantComposerLabels();
  return (
    <SharedComposer
      density="full"
      storeKey="home"
      block={block}
      seed={seed}
      onCommand={onCommand}
      onSubmitted={onSent}
      onSend={({ body, refs, attachments, mediaRefs }) => {
        void sendToMaster(body, newMessageId(), refs, attachments, attachDestination?.conversation, undefined, mediaRefs);
      }}
      refocusAfterSend
      attach={{
        enabled: attachEnabled,
        reason: attachReason,
        destination: attachDestination,
      }}
      className="card p-2.5 relative"
      placeholder={placeholder}
      ariaLabel={ariaLabel}
      // Home's `data-composer` hook — tests and the dock find the form by it.
      formProps={{ "data-composer": "" }}
    />
  );
}
