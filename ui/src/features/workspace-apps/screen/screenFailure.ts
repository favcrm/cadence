/**
 * The one vocabulary the app pane explains itself with. A mount either
 * reaches `ready` or it does not; when it does not, the outlet reports
 * exactly why so the pane can offer the one action that can change the
 * outcome instead of a generic fallback.
 *
 * This module is pure — no React, no network — so the copy and the action
 * set are testable without mounting a frame.
 */

/** A mount the outlet could not bring to `ready`. */
export type ScreenFailure =
  | { kind: "refused"; status: number; code: string | null }
  | { kind: "invalid_receipt" }
  | { kind: "handshake_timeout" }
  | { kind: "frame_unavailable" };

/** Everything the pane is asked to explain. `unsupported` is decided before
 *  a mount; `needs_host_update` is the receipt's `min_shell` above the shell. */
export type ScreenPaneState =
  | ScreenFailure
  | { kind: "needs_host_update"; required: string; current: string }
  | { kind: "unsupported" };

export type ScreenPaneAction = "retry" | "sign_in" | "reinstall" | "check_update";

/** `title` is the English locale key; the pane translates it. */
export interface ScreenPane { title: string; actions: ScreenPaneAction[] }

const SESSION_ENDED = "Your session ended. Sign in again.";
const FILES_MISSING = "This app's files are not on this board yet.";
const SCREEN_MISSING = "This app's screen is not available on this board.";
const DID_NOT_LOAD = "The app did not finish loading.";
const UNSUPPORTED = "This installation has no supported board workspace yet. Use its installed guide and the app commands to inspect supported workflows.";
const NEEDS_NEWER = "This app needs a newer board (needs {required}, this board is {current}).";

function refusedPane(status: number, code: string | null, installable: boolean): ScreenPane {
  // A 401 is the session ending; the structured codes name the same two
  // refusals on other statuses, so both read as one "sign in again".
  if (status === 401 || code === "board_session_required" || code === "operator_session_required") {
    return { title: SESSION_ENDED, actions: ["sign_in"] };
  }
  if (code === "app_bundle_missing") {
    return { title: FILES_MISSING, actions: installable ? ["retry", "reinstall"] : ["retry"] };
  }
  if (code === "screen_unavailable" || code === "not_found") {
    return { title: SCREEN_MISSING, actions: ["retry"] };
  }
  return { title: DID_NOT_LOAD, actions: ["retry"] };
}

/**
 * The pane for one state. `installable` says whether this app has a source
 * the board can install from — only then does a missing bundle offer
 * `Reinstall`; otherwise `Retry` is the whole action set.
 */
export function screenFailurePane(failure: ScreenPaneState, installable = false): ScreenPane {
  switch (failure.kind) {
    case "refused":
      return refusedPane(failure.status, failure.code, installable);
    case "needs_host_update":
      return { title: NEEDS_NEWER, actions: ["check_update"] };
    case "unsupported":
      return { title: UNSUPPORTED, actions: ["retry"] };
    default:
      return { title: DID_NOT_LOAD, actions: ["retry"] };
  }
}
