/**
 * CAD-2269: minimal personal assistant setup card (Avatar + Name + Skip for now),
 * hosted About-panel direct save with older-board 501 fallback, and hosted SetupNudge skip.
 */
import type { AssistantActorValue } from "../src/features/assistant/AssistantIdentity";
import type { AvatarLook, IdentityRecord, PersonalProfileOutcome } from "../src/features/assistant/identity";

declare function require(name: string): any;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/" });
  for (const name of [
    "window",
    "document",
    "Node",
    "Element",
    "HTMLElement",
    "HTMLInputElement",
    "HTMLButtonElement",
    "SVGElement",
    "navigator",
    "MutationObserver",
    "ResizeObserver",
    "Event",
    "MouseEvent",
    "KeyboardEvent",
    "location",
    "history",
    "localStorage",
    "sessionStorage",
  ]) {
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  }
  for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"]) {
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  }
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

  const loader = require("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, arguments);
  };

  const React = require("react") as typeof import("react");
  const { act } = React;
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const {
    AssistantAboutButton,
    AssistantActorProvider,
    AssistantBootstrapCard,
    toPersonalStage1Mutation,
    toPersonalUpdateMutation,
  } = require("../src/features/assistant/AssistantIdentity") as typeof import("../src/features/assistant/AssistantIdentity");
  const SetupNudge = (require("../src/features/setup/SetupNudge") as typeof import("../src/features/setup/SetupNudge")).default;
  const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof import("../screen-ui/src/navigation");
  const { hostNavigation } = require("../src/ui/hostNavigation") as typeof import("../src/ui/hostNavigation");

  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);

  // 1. Hosted SetupNudge skips mounting / fetching GET /api/setup.
  let fetchCount = 0;
  const originalFetch = globalThis.fetch;
  globalThis.fetch = (async () => {
    fetchCount += 1;
    throw new Error("unexpected fetch on hosted SetupNudge");
  }) as typeof fetch;
  await act(async () => {
    root.render(React.createElement(SetupNudge, { readOnly: false, hosted: true }));
  });
  assert(host.innerHTML === "" && fetchCount === 0, "hosted SetupNudge renders nothing and skips /api/setup");
  globalThis.fetch = originalFetch;

  // 2. AssistantBootstrapCard renders ONLY Avatar (11 swatches) + Name + Skip for now + primary CTA.
  const initialIdentity: IdentityRecord = { name: null, tone: null, look: "robot", revision: 0 };
  const completions: Array<{ expected_revision: number; name?: string; look: AvatarLook }> = [];
  const defers: number[] = [];

  await act(async () => {
    root.render(
      React.createElement(AssistantBootstrapCard, {
        key: "supported",
        identity: initialIdentity,
        onComplete: async (input): Promise<PersonalProfileOutcome> => {
          completions.push(input);
          return {
            ok: true,
            identity: { name: input.name ?? null, tone: null, look: input.look, revision: 1 },
            bootstrap: "complete",
            conversation: "home-1",
            alias: "assistant",
          };
        },
        onDefer: async (rev): Promise<PersonalProfileOutcome> => {
          defers.push(rev);
          return {
            ok: true,
            identity: { ...initialIdentity, revision: 1 },
            bootstrap: "deferred",
            conversation: "home-1",
            alias: "assistant",
          };
        },
      }),
    );
  });

  const lookButtons = Array.from(host.querySelectorAll<HTMLButtonElement>("[data-assistant-look]"));
  assert(lookButtons.length === 11, `expected 11 canonical avatar swatches, got ${lookButtons.length}`);
  assert(host.querySelectorAll("textarea").length === 0, "bootstrap card has no personality/preferences textarea");

  const guardBtn = host.querySelector<HTMLButtonElement>('[data-assistant-look="guard"]');
  assert(guardBtn !== null, "guard avatar swatch present");
  await act(async () => {
    guardBtn.click();
  });

  const novaChip = Array.from(host.querySelectorAll<HTMLButtonElement>("button.assistant-name-chip")).find(
    (btn) => btn.textContent === "Nova",
  );
  assert(novaChip !== undefined, "Nova preset chip present");
  await act(async () => {
    novaChip.click();
  });

  const submitBtn = host.querySelector<HTMLButtonElement>('button[type="submit"]');
  assert(submitBtn !== null && submitBtn.textContent?.includes("Start with Nova"), "CTA updates to Start with Nova");
  await act(async () => {
    submitBtn.click();
  });
  assert(
    completions.length === 1 &&
      completions[0]?.expected_revision === 0 &&
      completions[0]?.name === "Nova" &&
      completions[0]?.look === "guard",
    "submit sends fenced payload with chosen name and look",
  );
  const stage1Req = toPersonalStage1Mutation(completions[0]!);
  assert(
    stage1Req.action === "update" && stage1Req.name === "Nova" && stage1Req.look === "guard",
    "Stage 1 with a chosen name keeps bootstrap_state pending via action: update for Stage 2 chat onboarding",
  );
  const toneReq = toPersonalUpdateMutation({ expected_revision: 1, tone: "Concise" });
  assert(toneReq.tone === "Concise", "personal update mutation includes tone when provided");

  const skipBtn = Array.from(host.querySelectorAll<HTMLButtonElement>("button")).find(
    (btn) => btn.textContent === "Skip for now",
  );
  assert(skipBtn !== undefined, "Skip for now button present");
  await act(async () => {
    skipBtn.click();
  });
  assert(defers.length === 1 && defers[0] === 0, "Skip for now sends defer with current revision");

  // 2b. Older-server 404/501 on Skip for now stays fail-closed (ok: false, pending, alert shown, controls disabled).
  await act(async () => {
    root.render(
      React.createElement(AssistantBootstrapCard, {
        key: "unsupported",
        identity: initialIdentity,
        onComplete: async (): Promise<PersonalProfileOutcome> => ({
          ok: false,
          unsupported: true,
          error: "Direct assistant setup isn’t available on this board yet. Say hello below to name your assistant.",
        }),
        onDefer: async (): Promise<PersonalProfileOutcome> => ({
          ok: false,
          unsupported: true,
          error: "Direct assistant setup isn’t available on this board yet. Say hello below to name your assistant.",
        }),
      }),
    );
  });
  const unsupportedSkipBtn = Array.from(host.querySelectorAll<HTMLButtonElement>("button")).find(
    (btn) => btn.textContent === "Skip for now",
  );
  assert(unsupportedSkipBtn !== undefined, "Skip for now button present on older-server card");
  await act(async () => {
    unsupportedSkipBtn.click();
  });
  assert(
    host.querySelector("[data-assistant-bootstrap='pending']") !== null,
    "404/501 defer keeps bootstrap pending rather than inventing deferred success",
  );
  assert(
    host.querySelector("[role='alert']")?.textContent?.includes("Direct assistant setup isn’t available") === true,
    "404/501 defer surfaces truthful unsupported alert",
  );
  assert(unsupportedSkipBtn.disabled, "404/501 defer disables retrying unsupported endpoint");

  // 3. Slim personal AssistantAboutDialog renders read-only details + profile link without inline identity editing.
  const renderAbout = async (profileHref?: string, withChildTrigger = false) => {
    const actor: AssistantActorValue = {
      scope: "personal",
      alias: "assistant",
      model: "agenticos/default",
      read: { state: "ready", identity: { name: "Nova", tone: "Concise & direct", look: "guard", revision: 2 } },
      personal: {
        pendingLook: null,
        requestLook: async () => ({ ok: true }),
      },
      workspace: null,
    };
    await act(async () => {
      root.render(
        React.createElement(NavigationProvider, {
          adapter: hostNavigation,
          children: React.createElement(AssistantActorProvider, {
            actor,
            children: React.createElement(
              AssistantAboutButton,
              profileHref ? { profileHref, className: withChildTrigger ? "assistant-identity-trigger" : "" } : {},
              withChildTrigger ? React.createElement("span", null, "Nova") : undefined,
            ),
          }),
        }),
      );
    });
  };

  await renderAbout("/agents/assistant?tab=instructions", true);
  const trigger = host.querySelector<HTMLButtonElement>(".assistant-about-trigger.assistant-identity-trigger");
  assert(trigger !== null && trigger.textContent === "Nova", "Identity trigger renders custom child label");
  await act(async () => {
    trigger.click();
  });

  const dialog = document.body.querySelector<HTMLElement>("[role='dialog']");
  assert(dialog !== null, "About dialog opened");
  assert(
    dialog.querySelector("[data-assistant-name]")?.textContent === "Nova" &&
      dialog.textContent?.includes("Concise & direct") === true &&
      dialog.textContent?.includes("agenticos/default") === true,
    "Slim About dialog displays committed name, tone, and model",
  );
  assert(
    dialog.querySelector("input.field") === null &&
      dialog.querySelector("[data-assistant-look]") === null &&
      dialog.querySelector('button[type="submit"]') === null,
    "Slim personal About dialog omits inline avatar, name, and tone editor controls",
  );
  const profileLink = dialog.querySelector<HTMLAnchorElement>("a[href='/agents/assistant?tab=instructions']");
  assert(profileLink !== null, "Slim About dialog links to /agents/assistant?tab=instructions");

  await act(async () => {
    dialog.querySelector<HTMLButtonElement>("button.s-drawer-close")?.click();
  });
  await renderAbout(undefined, false);
  const infoTrigger = host.querySelector<HTMLButtonElement>(".assistant-about-trigger");
  assert(infoTrigger !== null, "Fallback About trigger rendered");
  await act(async () => {
    infoTrigger.click();
  });
  const dialogNoLink = document.body.querySelector<HTMLElement>("[role='dialog']");
  assert(
    dialogNoLink !== null &&
      dialogNoLink.textContent?.includes("Ask me in this conversation") === true &&
      dialogNoLink.querySelector("a[href]") === null,
    "Personal About dialog without profileHref falls back to chat guidance note",
  );

  await act(async () => {
    root.unmount();
  });
  console.log("CAD-2269 personal assistant bootstrap and slim About drawer passed");
}

void main();
