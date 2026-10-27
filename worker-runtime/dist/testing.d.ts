import { authorizeTool, authorizeTools } from "./tool-policy.js";
import type { ToolAuthority } from "./tool-policy.js";
/**
 * `@cadence/hosted-runtime/testing`: the real guards an application test needs
 * and opaque record fixtures for the engine's stored and live shapes.
 *
 * This entry imports no engine SDK and no Workers platform module. The fixture
 * builders return `unknown`, so a consumer never handles a private record type
 * and the engine's own shapes stay inside this package.
 */
export { authorizeTool, authorizeTools };
export type { ToolAuthority };
/** The assistant parts a fixture record may carry. */
export type AssistantFixtureContent = readonly ({
    type: "text";
    text: string;
} | {
    type: "thinking";
    thinking: string;
    redacted?: boolean;
})[];
/** One stored operator input, as the engine persists it. */
export declare function userRecord(id: number, text: string): unknown;
/** One stored completed assistant turn, as the engine persists it. */
export declare function assistantRecord(id: number, content: AssistantFixtureContent): unknown;
/**
 * The engine's committed batch for one live assistant turn: a thinking delta
 * and block per thinking part, then the committed message. A test hands this
 * to a stream listener so the real transcript mapping runs over live records.
 */
export declare function liveBatch(entry: unknown): readonly unknown[];
