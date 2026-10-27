declare function require(name: string): any;
export {};
/**
 * CAD-1211: when the session stops being the operator, `useInstallations`
 * is switched off and must drop the operator's last list, error and retry
 * state. Re-enabling loads again.
 */
function equal(a: unknown, e: unknown, why: string) {
  if (JSON.stringify(a) !== JSON.stringify(e)) throw new Error(`${why}: expected ${JSON.stringify(e)}, got ${JSON.stringify(a)}`);
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "navigator", "location"])
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  const React = require("react") as typeof import("react");
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const { act } = React;
  const { workspaceApps } = require("../src/features/workspace-apps/workspaceApps") as typeof import("../src/features/workspace-apps/workspaceApps");
  const { useInstallations } = require("../src/features/workspace-apps/useInstallations") as typeof import("../src/features/workspace-apps/useInstallations");

  const operatorList = [{ id: "install-notes" }];
  let calls = 0;
  let lastSignal: AbortSignal | undefined;
  (workspaceApps as any).installations = async (signal?: AbortSignal) => {
    calls += 1;
    lastSignal = signal;
    return operatorList;
  };

  let seen: ReturnType<typeof useInstallations> | undefined;
  function Probe({ enabled }: { enabled: boolean }) {
    seen = useInstallations(enabled, "k");
    return null;
  }
  const root = createRoot(win.document.createElement("div"));
  const settle = () => act(async () => { await new Promise((r) => setTimeout(r, 10)); });

  await act(async () => { root.render(React.createElement(Probe, { enabled: true })); });
  await settle();
  equal(seen?.list, operatorList, "operator list loaded while enabled");
  equal(calls, 1, "one request while enabled");

  await act(async () => { root.render(React.createElement(Probe, { enabled: false })); });
  await settle();
  equal(
    { list: seen?.list, error: seen?.error, retrying: seen?.retrying, loading: seen?.loading },
    { list: null, error: null, retrying: false, loading: false },
    "disabled hook shows nothing of the operator's last state",
  );
  equal(lastSignal?.aborted, true, "the enabled request is aborted on disable");

  await act(async () => { root.render(React.createElement(Probe, { enabled: true })); });
  await settle();
  equal(calls, 2, "re-enabling requests again");
  equal(seen?.list, operatorList, "re-enabled hook loads the list again");

  await act(async () => { root.unmount(); });
  win.close?.();
}
void main().then(() => console.log("installations disable passed"));
