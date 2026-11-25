import type { ReactNode } from "react";
import type { ThreadRef } from "../../lib/types";
import AssistantComposer, { type AssistantModelPickerProps } from "../app-shell/chat/AssistantComposer";
import type { SlashCommand } from "./master";
import type { AttachDestination } from "../app-shell/chat/attach";
import type { MentionCandidate } from "../mentions/mentions";
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
  commands,
  onSent,
  attachEnabled = true,
  attachDestination,
  attachReason = "A writable Home conversation is required to attach files.",
  stop = null,
  textareaId,
  onArrowUpEmpty,
  mentionCandidates,
  allowNote,
  onDraftChange,
  beforeSend,
  modelPicker,
  modelControls,
}: {
  block: string | null;
  seed: { text: string; n: number; refs?: ThreadRef[] };
  onCommand: (name: string, arg: string) => boolean | void;
  commands?: readonly SlashCommand[];
  /** Enter or the button actually submitted — chat, slash, or an Ask-master draft. */
  onSent: () => void;
  attachEnabled?: boolean;
  attachDestination?: AttachDestination;
  attachReason?: string;
  /** CAD-1880: this pane's running turn — Stop takes the send slot. */
  stop?: { onStop: () => void } | null;
  textareaId?: string;
  onArrowUpEmpty?: () => boolean;
  mentionCandidates?: MentionCandidate[];
  allowNote?: boolean;
  onDraftChange?: (draft: string) => void;
  beforeSend?: (body: string) => boolean;
  modelPicker?: AssistantModelPickerProps | null;
  modelControls?: ReactNode;
}) {
  const { placeholder, ariaLabel } = useAssistantComposerLabels();
  return (
    <AssistantComposer
      density="full"
      storeKey="home"
      block={block}
      seed={seed}
      onCommand={onCommand}
      commands={commands}
      onSubmitted={onSent}
      onSend={({ body, refs, attachments, mediaRefs }) => {
        void sendToMaster(body, newMessageId(), refs, attachments, attachDestination?.conversation, undefined, mediaRefs);
      }}
      refocusAfterSend
      stop={stop}
      textareaId={textareaId}
      onArrowUpEmpty={onArrowUpEmpty}
      mentionCandidates={mentionCandidates}
      allowNote={allowNote}
      onDraftChange={onDraftChange}
      beforeSend={beforeSend}
      modelPicker={modelPicker}
      modelControls={modelControls}
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
