import type { ComponentProps, ReactNode } from "react";
import Composer, { type ComposerProps } from "./Composer";
import ComposerModelControls from "./ComposerModelControls";

export type AssistantModelPickerProps = ComponentProps<typeof ComposerModelControls>;

export interface AssistantComposerProps extends Omit<ComposerProps, "modelControls"> {
  /** Capability-driven model and thinking-effort picker configuration. */
  modelPicker?: AssistantModelPickerProps | null;
  /** Optional custom node override when callers construct model controls themselves. */
  modelControls?: ReactNode;
}

/**
 * CAD-2811: Unified capability-driven AI assistant composer shared across
 * Owner Home, Member Home, and App Chat. Renders one consistent composer frame,
 * attachment workflow, model picker, and thinking-effort pill driven by props
 * and server capabilities rather than role-specific branching.
 */
export default function AssistantComposer({
  modelPicker,
  modelControls,
  ...composerProps
}: AssistantComposerProps) {
  return (
    <Composer
      {...composerProps}
      modelControls={modelPicker ? <ComposerModelControls {...modelPicker} /> : modelControls}
    />
  );
}
