import type { RuntimeTool, RuntimeTools, ToolContext } from "./contract.js";
/** The application-owned policy the runtime consults for every call. */
export interface ToolAuthority {
    /** The application's refusal for this tool name, or `undefined`. */
    toolRefusal(name: string): string | undefined;
    /** Revalidate the live authority of this conversation. */
    authorizeTool(name: string): Promise<void>;
}
/**
 * The one execution order: refusal, abort, live authorization, abort, then the
 * tool itself. A refusal or an aborted signal never reaches authorization or
 * the tool, and authorization runs again on every attempt, including a replay
 * whose intent is already durable.
 */
export declare function withToolAuthority<T>(name: string, authority: ToolAuthority, context: ToolContext, execute: () => Promise<T>): Promise<T>;
/** Wrap one declared tool with the execution order above. */
export declare function authorizeTool(tool: RuntimeTool, authority: ToolAuthority): RuntimeTool;
/** Wrap one named group; the group's name and order are preserved. */
export declare function authorizeTools(group: RuntimeTools, authority: ToolAuthority): RuntimeTools;
