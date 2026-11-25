import { useCallback, useEffect, useMemo } from "react";
import { Resource as CoreResource } from "../../../lib/cacheCore";
import { resources } from "../../../lib/resources";
import { useQuery } from "../../../lib/useResource";
import type { Agent, MasterState } from "../../../lib/types";
import { offeredCommands, type SlashCommand } from "../../home/master";
import type { AssistantModelPickerProps } from "./AssistantComposer";

const IDLE_MASTER_RESOURCE = new CoreResource<MasterState>(
  () => new Promise<MasterState>(() => undefined),
  {},
  () => false,
);

function hasPickerCommands(commands: readonly string[] | undefined): boolean {
  return Array.isArray(commands) && (commands.includes("model") || commands.includes("effort"));
}

/**
 * True when an SSE `event: master` frame lacks capability commands (such as a
 * bare DO frame from a predating server build).
 */
export function isBareMasterStreamSnapshot(snapshot: MasterState): boolean {
  return Array.isArray(snapshot.commands) && !hasPickerCommands(snapshot.commands);
}

/**
 * Merge a live `event: master` snapshot onto canonical `GET /api/master/state`
 * data so a bare runtime stream frame cannot strip `commands`, `effort`, or a
 * resolved `model_label`, while enriched frames and turn state apply live.
 */
export function mergeMasterStreamSnapshot(
  prev: MasterState | null,
  snapshot: MasterState,
): MasterState {
  if (prev === null) return snapshot;
  const keepCommands = hasPickerCommands(prev.commands) && !hasPickerCommands(snapshot.commands);
  const preserveEffort =
    isBareMasterStreamSnapshot(snapshot) && snapshot.effort == null && prev.effort != null;
  const nextModel = snapshot.model ?? prev.model;
  const sameModel = nextModel === prev.model;
  const preserveLabel =
    sameModel &&
    typeof prev.model_label === "string" &&
    prev.model_label !== "" &&
    (snapshot.model_label == null || snapshot.model_label === snapshot.model);
  return {
    ...snapshot,
    model: nextModel,
    model_label: preserveLabel ? prev.model_label : snapshot.model_label,
    effort: preserveEffort ? prev.effort : snapshot.effort,
    commands: keepCommands ? prev.commands : (snapshot.commands ?? prev.commands),
  };
}

export interface UseAssistantModelStateOptions {
  hosted: boolean | null | undefined;
  /** Pass `string | null` for conversation-scoped surfaces (Member Home, hosted App Chat), or omit for global Home. */
  conversationId?: string | null;
  /** Gate scoped fetches until surface identity/bootstrap confirms a writable conversation. */
  enabled?: boolean;
  /** Session/surface key whose change revalidates capabilities even across synthetic test reuse. */
  scopeKey?: string;
  row?: Agent;
  readOnly: boolean;
  compact?: boolean;
  /** Require a loaded `/api/master/state` payload before showing model/effort pills. */
  requireLoaded?: boolean;
  /** Require a non-null `conversationId` before showing model/effort pills. */
  requireConversation?: boolean;
}

export interface AssistantModelStateController {
  source: CoreResource<MasterState>;
  state: ReturnType<typeof useQuery<MasterState>>;
  offered: SlashCommand[];
  canPickModel: boolean;
  canPickEffort: boolean;
  canStop: boolean;
  refresh: () => void;
  applyStreamSnapshot: (snapshot: MasterState) => void;
  modelPicker: AssistantModelPickerProps;
}

/**
 * CAD-2817: Shared capability, model, effort, and stream-merge state for
 * Owner Home, Member Home, and App Chat.
 */
export function useAssistantModelState({
  hosted,
  conversationId,
  enabled = true,
  scopeKey,
  row,
  readOnly,
  compact = false,
  requireLoaded = false,
  requireConversation = false,
}: UseAssistantModelStateOptions): AssistantModelStateController {
  const scopedMode = conversationId !== undefined;
  const activeConversation = scopedMode && enabled && conversationId !== null ? conversationId : null;
  const source = scopedMode
    ? activeConversation !== null
      ? resources.masterConversation(activeConversation)
      : IDLE_MASTER_RESOURCE
    : resources.masterState;

  const state = useQuery(source);

  useEffect(() => {
    if (scopeKey === undefined) return;
    if (scopedMode ? activeConversation === null : !enabled) return;
    void source.revalidate();
  }, [scopedMode, activeConversation, enabled, source, scopeKey]);

  const master = scopedMode && activeConversation === null ? null : state.data;
  const offered = useMemo(
    () => offeredCommands(hosted, master?.commands),
    [hosted, master?.commands],
  );
  const readyForControls =
    (!requireLoaded || master !== null) && (!requireConversation || activeConversation !== null);
  const canPickModel = readyForControls && offered.some((c) => c.name === "model");
  const canPickEffort = readyForControls && offered.some((c) => c.name === "effort");
  const canStop = offered.some((c) => c.name === "stop");

  const refresh = useCallback(() => {
    if (scopedMode && activeConversation === null) return;
    void source.refresh();
  }, [scopedMode, activeConversation, source]);

  const applyStreamSnapshot = useCallback(
    (snapshot: MasterState) => {
      if (scopedMode && activeConversation === null) return;
      const hadData = source.get().data !== null;
      source.write((prev) => mergeMasterStreamSnapshot(prev, snapshot));
      if (!hadData || isBareMasterStreamSnapshot(snapshot)) {
        void source.invalidate();
      }
    },
    [scopedMode, activeConversation, source],
  );

  const modelPicker: AssistantModelPickerProps = {
    master,
    row,
    conversation: activeConversation ?? undefined,
    readOnly,
    canPickModel,
    canPickEffort,
    compact,
    onRefreshState: refresh,
  };

  return {
    source,
    state,
    offered,
    canPickModel,
    canPickEffort,
    canStop,
    refresh,
    applyStreamSnapshot,
    modelPicker,
  };
}
