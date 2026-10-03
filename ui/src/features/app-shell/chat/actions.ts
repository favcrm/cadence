import type { HostActionId } from "./registry";

/**
 * The host action registry for descriptor card buttons (CAD-1109). Host
 * code, ids only: an app cannot add one. v1 has no mutating action (D3), so
 * `open-view` is the only entry and it navigates the shell inside the pane's
 * own installation and context. Every parameter comes from the validated
 * descriptor, never from a message payload.
 */
export interface ActionContext {
  /** The shell's own navigation; the install and context stay the route's. */
  openView: (view: string) => void;
}

export function runHostAction(run: HostActionId, params: { view: string }, ctx: ActionContext): void {
  switch (run) {
    case "open-view":
      ctx.openView(params.view);
      return;
  }
}
