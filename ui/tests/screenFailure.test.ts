/**
 * The pane tells the truth: the receipt's optional `min_shell` is parsed
 * (and only that key is added to the closed parser), versions order
 * numerically, and every failure kind maps to one title key and the one
 * action that can change its outcome.
 */
import { compareShellVersions, parseMount } from "../src/features/workspace-apps/screen/screenLifecycle";
import { screenFailurePane, type ScreenPaneState } from "../src/features/workspace-apps/screen/screenFailure";

function check(value: unknown, label: string): void { if (!value) throw new Error(label); }
function equal(actual: unknown, expected: unknown, label: string) {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) throw new Error(`${label}: ${JSON.stringify(actual)} !== ${JSON.stringify(expected)}`);
}

const nonce = "a".repeat(64);
const receipt = { mount: `/api/app-screen/${"b".repeat(64)}`, bridge_nonce: nonce, generation: 2, tag: "main" };

// `min_shell` is the only new receipt key, and it must be a bare version.
const withMin = parseMount({ ...receipt, min_shell: "1.2.10" }, "main");
check(withMin && withMin.min_shell === "1.2.10", "a receipt with min_shell parses and carries it");
check(parseMount({ ...receipt, min_shell: "1.2.3" }, "main"), "a three-part min_shell parses");
check(!parseMount({ ...receipt, min_shell: "1.2.3", unexpected: true }, "main"), "min_shell does not open the closed receipt");
for (const bad of ["1.2", "v1.2.3", "1.2.3.4", "1.2.x", "", 3, {}, ["1.2.3"]]) {
  check(!parseMount({ ...receipt, min_shell: bad }, "main"), `min_shell refused: ${JSON.stringify(bad)}`);
}

// Numeric parts compare numerically: 10 is above 9, not below it.
equal(compareShellVersions("1.2.10", "1.2.9"), 1, "1.2.10 is above 1.2.9");
equal(compareShellVersions("1.2.9", "1.2.10"), -1, "1.2.9 is below 1.2.10");
equal(compareShellVersions("1.2.3", "1.2.3"), 0, "equal versions compare equal");

const SIGN_IN = "Your session ended. Sign in again.";
const FILES_MISSING = "This app's files are not on this board yet.";
const SCREEN_MISSING = "This app's screen is not available on this board.";
const DID_NOT_LOAD = "The app did not finish loading.";
const UNSUPPORTED = "This installation has no supported board workspace yet. Use its installed guide and the app commands to inspect supported workflows.";
const NEEDS_NEWER = "This app needs a newer board (needs {required}, this board is {current}).";

equal(screenFailurePane({ kind: "refused", status: 401, code: null }), { title: SIGN_IN, actions: ["sign_in"] }, "401 signs in");
equal(screenFailurePane({ kind: "refused", status: 400, code: "board_session_required" }), { title: SIGN_IN, actions: ["sign_in"] }, "board_session_required signs in");
equal(screenFailurePane({ kind: "refused", status: 503, code: "app_bundle_missing" }), { title: FILES_MISSING, actions: ["retry"] }, "a missing bundle retries");
equal(screenFailurePane({ kind: "refused", status: 503, code: "app_bundle_missing" }, true), { title: FILES_MISSING, actions: ["retry", "reinstall"] }, "an installable missing bundle also reinstalls");
equal(screenFailurePane({ kind: "refused", status: 404, code: "screen_unavailable" }), { title: SCREEN_MISSING, actions: ["retry"] }, "screen_unavailable retries");
equal(screenFailurePane({ kind: "refused", status: 404, code: "not_found" }), { title: SCREEN_MISSING, actions: ["retry"] }, "not_found retries");
equal(screenFailurePane({ kind: "refused", status: 503, code: "capability_unavailable" }), { title: DID_NOT_LOAD, actions: ["retry"] }, "an unclassified refusal retries");
for (const kind of ["invalid_receipt", "handshake_timeout", "frame_unavailable"] as const) {
  equal(screenFailurePane({ kind }), { title: DID_NOT_LOAD, actions: ["retry"] }, `${kind} retries`);
}
equal(screenFailurePane({ kind: "unsupported" }), { title: UNSUPPORTED, actions: ["retry"] }, "unsupported keeps the explanation and retries");
equal(screenFailurePane({ kind: "needs_host_update", required: "1.2.3", current: "1.0.0" }), { title: NEEDS_NEWER, actions: ["check_update"] }, "a newer board is a reload");

// The helper is total: every member of the union is handled, never thrown.
const states: ScreenPaneState[] = [
  { kind: "refused", status: 500, code: null },
  { kind: "invalid_receipt" },
  { kind: "handshake_timeout" },
  { kind: "frame_unavailable" },
  { kind: "needs_host_update", required: "9.9.9", current: "0.1.0" },
  { kind: "unsupported" },
];
for (const state of states) check(screenFailurePane(state).actions.length >= 1, `every pane offers an action: ${state.kind}`);

console.log("screen failure panes pass");
