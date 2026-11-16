export {};
/** CAD-1300: a member refusal must not stick after an authorized read. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const name of ["window", "document", "navigator", "HTMLElement", "location"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const { api, ApiError } = require("../src/lib/api");
const { useApproveHead } = require("../src/features/home/ApproveHead");
const host = document.createElement("div");
const root = createRoot(host);
let memberSession = true;
api.approvalState = async () => {
  if (memberSession) throw new ApiError("member", 403, { code: "member_role" });
  return { state: "missing" };
};
function Probe({ head }: { head: string }) {
  const approval = useApproveHead("https://github.com/acme/widgets/pull/42", head);
  return React.createElement("span", null, approval.member ? "member" : "owner");
}
async function main() {
  try {
    await React.act(async () => { root.render(React.createElement(Probe, { head: "a".repeat(40) })); });
    if (host.textContent !== "member") throw new Error("member refusal must disable owner actions");
    memberSession = false;
    await React.act(async () => { root.render(React.createElement(Probe, { head: "b".repeat(40) })); });
    if (String(host.textContent) !== "owner") throw new Error("successful owner read must clear the stale member restriction");

    // CAD-2311: 403 owner_required / approver_not_allowed on approvalState set member === true on initial read.
    for (const code of ["owner_required", "approver_not_allowed"] as const) {
      api.approvalState = async () => {
        throw new ApiError("owner required", 403, { code });
      };
      await React.act(async () => { root.render(React.createElement(Probe, { head: `${code[0].repeat(39)}1` })); });
      if (host.textContent !== "member") {
        throw new Error(`expected 403 ${code} read refusal to set member === true, got ${host.textContent}`);
      }
    }

    // CAD-2303: head changes clear stale in-force state immediately, optional context passes cleanly,
    // 400 from predating boards/daemons degrades to strict { repo, pr, head }, and 403/409 do not retry.
    const stateCalls: Array<{ repo: string; pr: number; head: string; context?: { issue?: string; project?: string } }> = [];
    const approveCalls: Array<{ repo: string; pr: number; head: string; context?: { issue?: string; project?: string } }> = [];
    let releaseSlowRead!: () => void;
    let slowHead = false;
    let degradeState400 = false;
    let approveMode: "400-then-ok" | "409" | "403" = "400-then-ok";
    let stateAfterApprove: "missing" | "in-force" = "missing";

    api.approvalState = async (repo: string, pr: number, head: string, context?: { issue?: string; project?: string }) => {
      stateCalls.push({ repo, pr, head, context });
      if (slowHead) {
        await new Promise<void>((resolve) => { releaseSlowRead = resolve; });
        return { state: "missing" };
      }
      if (degradeState400 && context) {
        throw new ApiError("unknown query key", 400, { code: "validation_failed" });
      }
      return {
        state: stateAfterApprove,
        ...(stateAfterApprove === "in-force" ? { approval_id: "apr_1", board_revocable: true } : {}),
        verdicts: [{ name: "standards", agent: "svc_review_standards", verdict: "PASS", sha: head }],
      };
    };
    api.approveHead = async (repo: string, pr: number, head: string, context?: { issue?: string; project?: string }) => {
      approveCalls.push({ repo, pr, head, context });
      if (approveMode === "400-then-ok" && context) {
        throw new ApiError("unsupported field", 400, { code: "validation_failed" });
      }
      if (approveMode === "409") {
        throw new ApiError("already approved", 409, { code: "already_approved" });
      }
      if (approveMode === "403") {
        throw new ApiError("forbidden", 403, { code: "approver_not_allowed" });
      }
      stateAfterApprove = "in-force";
      return { approval_id: "apr_1" };
    };

    let latest!: ReturnType<typeof useApproveHead>;
    function ContextProbe({ head, issue, project }: { head: string; issue?: string; project?: string }) {
      latest = useApproveHead("https://github.com/acme/widgets/pull/42", head, { issue, project });
      return React.createElement("span", null, `${latest.state}:${latest.approved ? "approved" : "not"}:${latest.verdicts.length}`);
    }

    // 400 fallback on approvalState + approveHead with context
    degradeState400 = true;
    await React.act(async () => {
      root.render(React.createElement(ContextProbe, { head: "c".repeat(40), issue: "CAD-2303", project: "cadence" }));
    });
    if (stateCalls.length !== 2 || stateCalls[0].context?.issue !== "CAD-2303" || stateCalls[1].context !== undefined) {
      throw new Error(`expected 400 state fallback to retry without context: ${JSON.stringify(stateCalls)}`);
    }
    degradeState400 = false;
    await React.act(async () => {
      root.render(React.createElement(ContextProbe, { head: "d".repeat(40), issue: "CAD-2303", project: "cadence" }));
    });
    if (latest.verdicts.length !== 1 || latest.verdicts[0].agent !== "svc_review_standards") {
      throw new Error(`expected board verdicts to parse: ${JSON.stringify(latest.verdicts)}`);
    }
    await React.act(async () => { latest.approve(); });
    if (approveCalls.length !== 2 || approveCalls[0].context?.project !== "cadence" || approveCalls[1].context !== undefined) {
      throw new Error(`expected 400 approve fallback to strict 3-tuple: ${JSON.stringify(approveCalls)}`);
    }
    if (!latest.approved) throw new Error("expected approved state after 400 fallback succeeded");

    // Head change immediately resets stale approved state even before slow read resolves
    slowHead = true;
    await React.act(async () => {
      root.render(React.createElement(ContextProbe, { head: "e".repeat(40), issue: "CAD-2303", project: "cadence" }));
    });
    if (latest.approved || latest.state !== "unknown" || latest.verdicts.length !== 0) {
      throw new Error(`stale head state must clear on head change: ${JSON.stringify(latest)}`);
    }
    releaseSlowRead();
    await React.act(async () => { await Promise.resolve(); });
    slowHead = false;

    // 409 / 403 must never trigger fallback retries
    for (const code of ["409", "403"] as const) {
      const before: number = approveCalls.length;
      approveMode = code;
      await React.act(async () => { latest.approve(); });
      if (approveCalls.length !== before + 1) {
        throw new Error(`HTTP ${code} must not retry approveHead (${approveCalls.length - before} calls)`);
      }
    }

    // CAD-2311: changing head/targetKey while a context-carrying approve is in-flight prevents the 400 fallback POST on the old key.
    let rejectInFlight400!: (err: unknown) => void;
    const baseApproveHead = api.approveHead;
    const beforeRace: number = approveCalls.length;
    api.approveHead = (repo: string, pr: number, head: string, context?: { issue?: string; project?: string }) => {
      approveCalls.push({ repo, pr, head, context });
      return new Promise((_resolve, reject) => { rejectInFlight400 = reject; });
    };
    await React.act(async () => {
      latest.approve();
      await Promise.resolve();
    });
    await React.act(async () => {
      root.render(React.createElement(ContextProbe, { head: "f".repeat(40), issue: "CAD-2311", project: "cadence" }));
    });
    api.approveHead = baseApproveHead;
    await React.act(async () => {
      rejectInFlight400(new ApiError("unsupported field", 400, { code: "validation_failed" }));
      await Promise.resolve();
      await Promise.resolve();
    });
    if (approveCalls.length !== beforeRace + 1) {
      throw new Error(`target change during 400 fallback must suppress retry (${approveCalls.length - beforeRace} calls)`);
    }

    // CAD-2332: Email-OTP step-up on `403 step_up_required`, refusal copy, and one-shot approve retry.
    const stepUpSendCalls: number[] = [];
    const stepUpVerifyCalls: Array<{ challenge_id: string; code: string }> = [];
    let nextSendStepUp: () => Promise<{ challenge_id: string; expires_at: number; email_hint: string }> = async () => {
      stepUpSendCalls.push(Date.now());
      return { challenge_id: "ch_stepup_1", expires_at: Date.now() + 600_000, email_hint: "o***@fav.hk" };
    };
    let nextVerifyStepUp: (challenge_id: string, code: string) => Promise<{ ok: true; authenticated_at: number; fresh_until: number }> = async (challenge_id, code) => {
      stepUpVerifyCalls.push({ challenge_id, code });
      return { ok: true, authenticated_at: Date.now(), fresh_until: Date.now() + 900_000 };
    };
    api.sendApprovalStepUp = () => nextSendStepUp();
    api.verifyApprovalStepUp = (challenge_id: string, code: string) => nextVerifyStepUp(challenge_id, code);

    let stepUpApproveBehavior: "require_then_ok" | "fresh_ok" | "policy_unavailable" = "require_then_ok";
    const stepUpApproveBodies: Array<{ repo: string; pr: number; head: string; context?: { issue?: string; project?: string; request_id?: string } }> = [];
    stateAfterApprove = "missing";
    api.approvalState = async () => ({
      state: stateAfterApprove,
      ...(stateAfterApprove === "in-force" ? { approval_id: "apr_stepup", board_revocable: true } : {}),
    });
    api.approveHead = async (repo: string, pr: number, head: string, context?: { issue?: string; project?: string; request_id?: string }) => {
      stepUpApproveBodies.push({ repo, pr, head, context });
      if (stepUpApproveBehavior === "require_then_ok" && stepUpVerifyCalls.length === 0) {
        throw new ApiError("Recent email verification is required before approving a merge.", 403, { code: "step_up_required" });
      }
      if (stepUpApproveBehavior === "policy_unavailable") {
        throw new ApiError("Policy missing", 403, { code: "project_policy_unavailable" });
      }
      stateAfterApprove = "in-force";
      return { approval_id: "apr_stepup" };
    };

    const headStepUp = "1".repeat(40);
    await React.act(async () => {
      root.render(React.createElement(ContextProbe, { head: headStepUp, issue: "CAD-2332", project: "cadence" }));
    });

    // 1) Stale session -> approve() opens OTP flow & auto-sends challenge without retrying approveHead.
    await React.act(async () => {
      await latest.approve();
    });
    if (!latest.stepUpOpen || latest.challenge?.email_hint !== "o***@fav.hk" || stepUpSendCalls.length !== 1 || stepUpApproveBodies.length !== 1) {
      throw new Error(`expected step_up_required to open OTP challenge and send once: ${JSON.stringify({ stepUpOpen: latest.stepUpOpen, challenge: latest.challenge, sends: stepUpSendCalls.length, approves: stepUpApproveBodies.length })}`);
    }
    if (latest.error !== null) {
      throw new Error(`opening step-up challenge should clear prior error, got ${latest.error}`);
    }

    // 2) Wrong code -> refuses with clear message, keeps challenge for retry, and does not retry approveHead.
    nextVerifyStepUp = async (challenge_id, code) => {
      stepUpVerifyCalls.push({ challenge_id, code });
      throw new ApiError("Verification code is invalid or expired.", 403, { code: "otp_invalid" });
    };
    await React.act(async () => { latest.setOtpCode("abc000000"); });
    await React.act(async () => { await latest.verifyAndApprove(); });
    if (
      latest.error !== "That 6-digit code did not match. Check the email and try again." ||
      latest.challenge === null ||
      latest.approved ||
      stepUpApproveBodies.length !== 1
    ) {
      throw new Error(`wrong OTP must keep challenge and not approve: ${JSON.stringify({ error: latest.error, challenge: latest.challenge, approved: latest.approved, approves: stepUpApproveBodies.length })}`);
    }

    // 3) Expired / exhausted code -> clears burned challenge so user can resend, still no approveHead call.
    for (const [status, code, expectedCopy] of [
      [403, "challenge_expired", "That verification code has expired. Send a new code and try again."],
      [429, "too_many_attempts", "Too many verification attempts. Wait a few minutes or send a new code."],
    ] as const) {
      await React.act(async () => {
        latest.clearError();
      });
      nextSendStepUp = async () => {
        stepUpSendCalls.push(Date.now());
        return { challenge_id: "ch_stepup_burned", expires_at: Date.now() + 600_000, email_hint: "o***@fav.hk" };
      };
      await React.act(async () => { await latest.sendStepUpCode(); });
      nextVerifyStepUp = async (challenge_id, otp) => {
        stepUpVerifyCalls.push({ challenge_id, code: otp });
        throw new ApiError("burned", status, { code });
      };
      await React.act(async () => { latest.setOtpCode("111111"); });
      await React.act(async () => { await latest.verifyAndApprove(); });
      if (latest.challenge !== null || latest.error !== expectedCopy || stepUpApproveBodies.length !== 1) {
        throw new Error(`burned challenge (${code}) must clear challenge and show '${expectedCopy}', got ${JSON.stringify({ challenge: latest.challenge, error: latest.error })}`);
      }
    }

    // 4) Valid code -> verifies and retries approveHead once with request_id, marking head approved.
    stepUpVerifyCalls.length = 0;
    nextSendStepUp = async () => {
      stepUpSendCalls.push(Date.now());
      return { challenge_id: "ch_stepup_2", expires_at: Date.now() + 600_000, email_hint: "o***@fav.hk" };
    };
    await React.act(async () => { await latest.sendStepUpCode(); });
    nextVerifyStepUp = async (challenge_id, code) => {
      stepUpVerifyCalls.push({ challenge_id, code });
      return { ok: true, authenticated_at: Date.now(), fresh_until: Date.now() + 900_000 };
    };
    await React.act(async () => { latest.setOtpCode("654321"); });
    await React.act(async () => { await latest.verifyAndApprove(); });
    const approveCountAfterVerify: number = stepUpApproveBodies.length;
    if (
      !latest.approved ||
      latest.stepUpOpen ||
      stepUpVerifyCalls.length !== 1 ||
      stepUpVerifyCalls[0].code !== "654321" ||
      approveCountAfterVerify !== 2 ||
      !stepUpApproveBodies[1].context?.request_id
    ) {
      throw new Error(`expected verified OTP to approve once with request_id: ${JSON.stringify({ approved: latest.approved, stepUpOpen: latest.stepUpOpen, verify: stepUpVerifyCalls, approves: stepUpApproveBodies })}`);
    }

    // 5) Fresh session (< 15m) -> approveHead succeeds with zero step-up prompts or calls.
    stateAfterApprove = "missing";
    stepUpApproveBehavior = "fresh_ok";
    const sendsBeforeFresh = stepUpSendCalls.length;
    await React.act(async () => {
      root.render(React.createElement(ContextProbe, { head: "2".repeat(40), issue: "CAD-2332", project: "cadence" }));
    });
    await React.act(async () => { await latest.approve(); });
    if (!latest.approved || latest.stepUpOpen || stepUpSendCalls.length !== sendsBeforeFresh) {
      throw new Error(`fresh session must approve without step-up prompt: ${JSON.stringify({ approved: latest.approved, stepUpOpen: latest.stepUpOpen })}`);
    }

    // 6) Resending inside cooldown (429 rate_limited) preserves the active challenge so the owner can still submit the received code;
    //    and a post-verify transient failure preserves request_id for the next approve() retry.
    await React.act(async () => {
      root.render(React.createElement(ContextProbe, { head: "6".repeat(40), issue: "CAD-2332", project: "cadence" }));
    });
    nextSendStepUp = async () => {
      stepUpSendCalls.push(Date.now());
      return { challenge_id: "ch_keep_active", expires_at: Date.now() + 600_000, email_hint: "o***@fav.hk" };
    };
    await React.act(async () => { await latest.sendStepUpCode(); });
    nextSendStepUp = async () => {
      throw new ApiError("rate limited", 429, { code: "rate_limited", retryAfter: 30 });
    };
    await React.act(async () => { await latest.sendStepUpCode(); });
    if (latest.challenge?.challenge_id !== "ch_keep_active" || latest.error !== "Too many verification emails requested. Wait 30s before resending.") {
      throw new Error(`resend refusal must preserve active challenge: ${JSON.stringify({ challenge: latest.challenge, error: latest.error })}`);
    }

    let postVerifyCalls = 0;
    let approvedAfterRetry = false;
    let firstReqId: string | undefined;
    let secondReqId: string | undefined;
    nextVerifyStepUp = async (challenge_id, code) => {
      stepUpVerifyCalls.push({ challenge_id, code });
      return { ok: true, authenticated_at: Date.now(), fresh_until: Date.now() + 900_000 };
    };
    api.approveHead = async (
      repo: string,
      pr: number,
      head: string,
      context?: { issue?: string; project?: string; request_id?: string },
    ) => {
      stepUpApproveBodies.push({ repo, pr, head, context });
      postVerifyCalls += 1;
      if (postVerifyCalls === 1) {
        firstReqId = context?.request_id;
        throw new ApiError("transient", 503, { code: "capability_unavailable" });
      }
      secondReqId = context?.request_id;
      approvedAfterRetry = true;
      stateAfterApprove = "in-force";
      return { approval_id: "apr_retry_ok" };
    };
    await React.act(async () => { latest.setOtpCode("654321"); });
    await React.act(async () => { await latest.verifyAndApprove(); });
    if (latest.stepUpOpen || !latest.error?.includes("Could not reach the approval service right now")) {
      throw new Error(`post-verify transient failure must close OTP box and show refusal, got ${JSON.stringify({ stepUpOpen: latest.stepUpOpen, error: latest.error })}`);
    }
    await React.act(async () => { await latest.approve(); });
    if (!approvedAfterRetry || !firstReqId || secondReqId !== firstReqId) {
      throw new Error(`post-verify approve retry must preserve request_id: ${JSON.stringify({ approvedAfterRetry, firstReqId, secondReqId })}`);
    }

    // 7) Plain refusal copy for rate_limited (with/without retryAfter), email_delivery_unavailable, capability_unavailable, and project_policy_unavailable.
    stateAfterApprove = "missing";
    api.approveHead = async (
      repo: string,
      pr: number,
      head: string,
      context?: { issue?: string; project?: string; request_id?: string },
    ) => {
      stepUpApproveBodies.push({ repo, pr, head, context });
      if (stepUpApproveBehavior === "policy_unavailable") {
        throw new ApiError("Policy missing", 403, { code: "project_policy_unavailable" });
      }
      stateAfterApprove = "in-force";
      return { approval_id: "apr_stepup" };
    };
    await React.act(async () => {
      root.render(React.createElement(ContextProbe, { head: "3".repeat(40), issue: "CAD-2332", project: "cadence" }));
    });
    for (const [err, expected] of [
      [new ApiError("rate", 429, { code: "rate_limited", retryAfter: 42 }), "Too many verification emails requested. Wait 42s before resending."],
      [new ApiError("rate", 429, { code: "rate_limited" }), "Too many verification emails requested. Wait a moment before resending."],
      [new ApiError("mail", 503, { code: "email_delivery_unavailable" }), "Could not send the verification email right now. Try again shortly."],
      [new ApiError("cap", 503, { code: "capability_unavailable" }), "Could not reach the approval service right now. Try again in a moment."],
    ] as const) {
      nextSendStepUp = async () => { throw err; };
      await React.act(async () => { await latest.sendStepUpCode(); });
      if (latest.error !== expected || /outage/i.test(latest.error ?? "")) {
        throw new Error(`expected plain message '${expected}', got '${latest.error}'`);
      }
    }
    stepUpApproveBehavior = "policy_unavailable";
    await React.act(async () => {
      root.render(React.createElement(ContextProbe, { head: "4".repeat(40), issue: "CAD-2332", project: "cadence" }));
    });
    await React.act(async () => { await latest.approve(); });
    if (latest.error !== "The owner records this project's access policy first (`cadence issue project policy cadence`).") {
      throw new Error(`expected project_policy_unavailable guidance with project key, got '${latest.error}'`);
    }
    await React.act(async () => {
      root.render(React.createElement(ContextProbe, { head: "5".repeat(40), issue: "CAD-2332" }));
    });
    await React.act(async () => { await latest.approve(); });
    if (latest.error !== "The owner records this project's access policy first (`cadence issue project policy <key>`).") {
      throw new Error(`expected <key> fallback when project is omitted, got '${latest.error}'`);
    }
    console.log("approval member-to-owner recovery passed");
  } finally {
    await React.act(async () => root.unmount());
  }
}
void main().catch((error: unknown) => { console.error(error); (globalThis as any).process.exitCode = 1; });
