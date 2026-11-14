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
    console.log("approval member-to-owner recovery passed");
  } finally {
    await React.act(async () => root.unmount());
  }
}
void main().catch((error: unknown) => { console.error(error); (globalThis as any).process.exitCode = 1; });
