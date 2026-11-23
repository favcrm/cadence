import { authorizeTool, authorizeTools } from "./tool-policy.js";
import type { ToolAuthority } from "./tool-policy.js";
/**
 * `@cadence/hosted-runtime/testing`: the real guards an application test needs
 * and opaque record fixtures for the engine's stored and live shapes.
 *
 * This entry imports no engine SDK and no Workers platform module. Record
 * builders return `unknown`, so consumers do not handle private record types.
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
type FixtureView = {
    readonly entries: readonly unknown[];
    readonly docs: {
        readonly "pi.live": Record<string, never>;
    };
};
type FixtureChange = {
    readonly kind: "value";
    readonly value: FixtureView;
    readonly ops: readonly [readonly ["r", FixtureView]];
    readonly retired: false;
};
type FixtureObserver = {
    advance(change: FixtureChange): void;
};
type FixtureEnd = {
    readonly kind: "ended";
} | {
    readonly kind: "listener_error";
    readonly error: unknown;
};
/**
 * A finite engine-shaped conversation for host integration tests. Records and
 * watch views remain opaque to the host, and this fixture never filters them.
 */
export declare function conversationFixture(stored: () => readonly unknown[], live: () => unknown | undefined): {
    session: () => {
        id: string;
        messages: () => Promise<readonly unknown[]>;
        busy: () => Promise<boolean>;
        watch: () => Promise<{
            readonly value: FixtureView;
            closed: Promise<FixtureEnd>;
            start(listener: (change: FixtureChange) => Promise<void>): void;
            stop(): Promise<{
                readonly kind: "ended";
            }>;
        }>;
    };
    conversation: () => Promise<{
        entries: () => Promise<{
            items: readonly unknown[];
        }>;
    }>;
    pending: () => Promise<never[]>;
    pi: () => Promise<{
        observeConversation: (_id: unknown, observer: FixtureObserver) => Promise<{
            value: FixtureView;
            detach: () => void;
        }>;
    }>;
};
