import type { IssueAssignmentRow } from "../src/lib/types";
import {
  buildCancelPayload,
  buildResumePayload,
  buildRetryPayload,
  canCancelLocalAssignment,
  canResumeLocalAssignment,
  canRetryLocalAssignment,
} from "../src/features/issues/hostedAssignmentControlsModel";

declare function require(name: string): any;
declare const process: { exit(code?: number): never };

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}

const RUNNER_ID = "lri_00000000-0000-4000-8000-000000000001";
const ASSIGN_ID = "hla_00000000-0000-4000-8000-000000000043";

function makeRow(over: Partial<IssueAssignmentRow> = {}): IssueAssignmentRow {
  return {
    assignment_id: ASSIGN_ID,
    issue_id: "DEMOCOMPANY-43",
    action: "dispatch",
    state: "blocked",
    generation: 2,
    target: "usr_owner",
    text: "Agent is fenced",
    spawn: {
      runner_id: RUNNER_ID,
      provider: "omp",
      model: "claude-sonnet-4",
      effort: "high",
      repo: "fav-limited/cadence",
    },
    updated_at: "2026-11-24T17:00:00Z",
    ...over,
  };
}

// 1. Pure model eligibility & payload builders
{
  const blockedSpawn = makeRow({ state: "blocked" });
  assert(canCancelLocalAssignment(blockedSpawn, true), "owner can cancel blocked assignment");
  assert(canRetryLocalAssignment(blockedSpawn, true), "owner can retry blocked assignment");
  assert(!canResumeLocalAssignment(blockedSpawn, true), "cannot resume non-terminal assignment");

  assert(!canCancelLocalAssignment(blockedSpawn, false), "non-owner cannot cancel");
  assert(!canRetryLocalAssignment(blockedSpawn, false), "non-owner cannot retry");
  assert(!canResumeLocalAssignment(blockedSpawn, false), "non-owner cannot resume");

  const runningSeat = makeRow({ state: "running", spawn: undefined, target_agent_id: "lag_1" });
  assert(canCancelLocalAssignment(runningSeat, true), "owner can cancel running seat");
  assert(!canRetryLocalAssignment(runningSeat, true), "cannot retry running seat");
  assert(!canResumeLocalAssignment(runningSeat, true), "cannot resume running seat");

  const doneSeat = makeRow({ state: "result", spawn: undefined, target_agent_id: "lag_1" });
  assert(!canResumeLocalAssignment(doneSeat, true), "non-spawn terminal row without runner_id is not resumable");

  const doneSpawn = makeRow({ state: "result" });
  assert(canResumeLocalAssignment(doneSpawn, true), "owner can resume completed spawned lane");

  const cancelledResumed = makeRow({
    state: "cancelled",
    action: "resume",
    spawn: undefined,
    resume: { runner_id: RUNNER_ID, prior_assignment_id: ASSIGN_ID, lane_alias: "omp-a1b2c3" },
  });
  assert(canResumeLocalAssignment(cancelledResumed, true), "owner can resume cancelled resumed lane");

  equal(
    buildCancelPayload(blockedSpawn, "op_cancel_1"),
    {
      assignment_id: ASSIGN_ID,
      expected_generation: 2,
      operation_id: "op_cancel_1",
    },
    "cancel payload",
  );

  equal(
    buildRetryPayload(blockedSpawn, "op_retry_1", {
      note: "  retrying with lighter effort  ",
      model: "gpt-5",
      effort: "medium",
    }),
    {
      assignment_id: ASSIGN_ID,
      expected_generation: 2,
      operation_id: "op_retry_1",
      note: "retrying with lighter effort",
      model: "gpt-5",
      effort: "medium",
    },
    "retry payload on spawn row",
  );

  const blockedSeat = makeRow({ state: "blocked", spawn: undefined, target_agent_id: "lag_1" });
  equal(
    buildRetryPayload(blockedSeat, "op_retry_2", {
      note: "seat note",
      model: "should-be-omitted",
      effort: "high",
    }),
    {
      assignment_id: ASSIGN_ID,
      expected_generation: 2,
      operation_id: "op_retry_2",
      note: "seat note",
    },
    "retry payload omits model/effort for non-spawn row",
  );

  equal(
    buildResumePayload(doneSpawn, "op_resume_1", {
      note: "address review feedback",
      model: "claude-opus-4",
      effort: "max",
    }),
    {
      idempotency_key: "op_resume_1",
      mode: "resume",
      resume: {
        runner_id: RUNNER_ID,
        prior_assignment_id: ASSIGN_ID,
        model: "claude-opus-4",
        effort: "max",
      },
      note: "address review feedback",
    },
    "resume payload",
  );
}

// 2. Mounted DOM rendering & fetch/API verification
async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/projects/demo-company/issues/DEMOCOMPANY-43" });
  for (const name of [
    "window",
    "document",
    "Node",
    "Element",
    "HTMLElement",
    "HTMLInputElement",
    "HTMLTextAreaElement",
    "HTMLSelectElement",
    "HTMLButtonElement",
    "Event",
    "MouseEvent",
    "KeyboardEvent",
    "location",
    "history",
  ]) {
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  }
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

  const loader = require("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    return originalRequire.apply(this, arguments);
  };

  const React = require("react") as typeof import("react");
  const { act } = React as unknown as { act: (fn: () => Promise<void> | void) => Promise<void> };
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
  const { api } = require("../src/lib/api") as typeof import("../src/lib/api");
  const HostedLocalAssignmentCard = (
    require("../src/features/issues/HostedLocalAssignmentCard") as typeof import("../src/features/issues/HostedLocalAssignmentCard")
  ).default;

  const origCancel = api.localDispatchCancel;
  const origRetry = api.localDispatchRetry;
  const origDispatch = api.localDispatch;

  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);

  const clickButton = async (label: string) => {
    const btn = Array.from(container.querySelectorAll("button")).find(
      (b) => b.textContent?.trim() === label,
    );
    assert(btn, `expected button '${label}'`);
    await act(async () => {
      btn.click();
    });
  };

  try {
    // A. Non-owner sees state/text only and zero lifecycle buttons
    await act(async () => {
      root.render(
        React.createElement(
          LocaleProvider,
          null,
          React.createElement(HostedLocalAssignmentCard, {
            issueId: "DEMOCOMPANY-43",
            assignment: makeRow({ state: "blocked" }),
            canLocalDispatch: false,
            readOnly: false,
          }),
        ),
      );
    });
    assert(!container.querySelector('[data-testid="hosted-assignment-actions"]'), "non-owner must not see actions");

    // B. ReadOnly owner sees zero lifecycle buttons
    await act(async () => {
      root.render(
        React.createElement(
          LocaleProvider,
          null,
          React.createElement(HostedLocalAssignmentCard, {
            issueId: "DEMOCOMPANY-43",
            assignment: makeRow({ state: "blocked" }),
            canLocalDispatch: true,
            readOnly: true,
          }),
        ),
      );
    });
    assert(!container.querySelector('[data-testid="hosted-assignment-actions"]'), "readOnly owner must not see actions");

    // C. Owner blocked -> Retry + Cancel; Retry submits payload, guards double-submit
    const calls: { kind: string; id: string; body: unknown }[] = [];
    let releaseRetry: (() => void) | null = null;
    api.localDispatchRetry = (id, body) => {
      calls.push({ kind: "retry", id, body });
      return new Promise((resolve) => {
        releaseRetry = () =>
          resolve({
            assignment: { assignment_id: ASSIGN_ID, issue_id: id, state: "queued", generation: 3 },
            replayed: false,
          });
      });
    };
    let mutatedCount = 0;
    const toasts: string[] = [];
    await act(async () => {
      root.render(
        React.createElement(
          LocaleProvider,
          null,
          React.createElement(HostedLocalAssignmentCard, {
            issueId: "DEMOCOMPANY-43",
            assignment: makeRow({ state: "blocked" }),
            canLocalDispatch: true,
            readOnly: false,
            onMutated: () => {
              mutatedCount += 1;
            },
            onToast: (msg: string) => toasts.push(msg),
          }),
        ),
      );
    });
    await clickButton("Retry");
    assert(container.querySelector('[data-testid="hosted-assignment-retry-form"]'), "retry form opens");

    const confirmRetry = Array.from(container.querySelectorAll("button")).find(
      (b) => b.textContent?.trim() === "Confirm retry",
    ) as HTMLButtonElement | undefined;
    assert(confirmRetry, "Confirm retry button present");
    await act(async () => {
      confirmRetry.click();
      confirmRetry.click();
    });
    assert(calls.length === 1, "in-flight guard prevents duplicate retry submit");
    equal((calls[0]!.body as Record<string, unknown>).expected_generation, 2, "retry generation");
    assert(
      String((calls[0]!.body as Record<string, unknown>).operation_id).startsWith("op_"),
      "operation_id uses op_ prefix",
    );
    await act(async () => {
      releaseRetry?.();
    });
    assert(mutatedCount === 1, "onMutated called after retry");
    assert(toasts.length === 1, "onToast called after retry");

    // D. Owner running -> Cancel confirms and posts localDispatchCancel
    api.localDispatchCancel = async (id, body) => {
      calls.push({ kind: "cancel", id, body });
      return {
        assignment: { assignment_id: ASSIGN_ID, issue_id: id, state: "cancelled", generation: 3 },
        replayed: false,
      };
    };
    await act(async () => {
      root.render(
        React.createElement(
          LocaleProvider,
          null,
          React.createElement(HostedLocalAssignmentCard, {
            issueId: "DEMOCOMPANY-43",
            assignment: makeRow({ state: "running" }),
            canLocalDispatch: true,
            readOnly: false,
            onMutated: () => {
              mutatedCount += 1;
            },
          }),
        ),
      );
    });
    assert(
      !Array.from(container.querySelectorAll("button")).some((b) => b.textContent?.trim() === "Retry"),
      "Retry hidden while running",
    );
    await clickButton("Cancel");
    await clickButton("Confirm cancel");
    assert((calls as { kind: string }[]).length === 2 && calls[1]!.kind === "cancel", "cancel endpoint called");

    // E. Owner terminal result -> Resume form error keeps guidance, then succeeds
    let failResume = true;
    api.localDispatch = async (id, body) => {
      calls.push({ kind: "resume", id, body });
      if (failResume) throw new Error("Runner offline");
      return {
        assignment: {
          assignment_id: "hla_00000000-0000-4000-8000-000000000044",
          issue_id: id,
          action: "resume",
          state: "queued",
          generation: 1,
        },
        replayed: false,
      };
    };
    await act(async () => {
      root.render(
        React.createElement(
          LocaleProvider,
          null,
          React.createElement(HostedLocalAssignmentCard, {
            issueId: "DEMOCOMPANY-43",
            assignment: makeRow({ state: "result" }),
            canLocalDispatch: true,
            readOnly: false,
          }),
        ),
      );
    });
    await clickButton("Resume lane");
    const textarea = container.querySelector("textarea") as HTMLTextAreaElement;
    assert(textarea, "resume note textarea present");
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(
        win.HTMLTextAreaElement.prototype,
        "value",
      )!.set!;
      setter.call(textarea, "Follow-up test");
      textarea.dispatchEvent(new win.Event("input", { bubbles: true }));
    });
    await clickButton("Send resume");
    const alert = container.querySelector('[role="alert"]');
    assert(alert?.textContent?.includes("Runner offline"), "error alert renders");
    equal(textarea.value, "Follow-up test", "failed resume retains note input");

    failResume = false;
    await clickButton("Send resume");
    const last = calls[calls.length - 1]!;
    assert(last.kind === "resume", "resume called api.localDispatch");
    equal((last.body as Record<string, unknown>).mode, "resume", "mode=resume sent");
    equal((last.body as Record<string, unknown>).note, "Follow-up test", "follow-up note sent");
  console.log("CAD-2777 hosted assignment controls tests passed");
  } finally {
    api.localDispatchCancel = origCancel;
    api.localDispatchRetry = origRetry;
    api.localDispatch = origDispatch;
    await act(async () => {
      root.unmount();
    });
    container.remove();
  }
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
