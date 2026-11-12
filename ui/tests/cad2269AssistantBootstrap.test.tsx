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
  } = require("../src/features/assistant/AssistantIdentity") as typeof import("../src/features/assistant/AssistantIdentity");
  const SetupNudge = (require("../src/features/setup/SetupNudge") as typeof import("../src/features/setup/SetupNudge")).default;

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
    "submit sends fenced complete payload with chosen name and look",
  );

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

  // 3. Hosted AssistantAboutDialog saves Name + Avatar directly, and falls back to requestLook on 501.
  const chatLookRequests: AvatarLook[] = [];
  let simulateUnsupported = false;
  const savedUpdates: Array<{ expected_revision: number; name?: string | null; look?: AvatarLook }> = [];

  const renderAbout = async () => {
    const actor: AssistantActorValue = {
      scope: "personal",
      alias: "assistant",
      read: { state: "ready", identity: { name: "Nova", tone: null, look: "guard", revision: 2 } },
      personal: {
        pendingLook: null,
        requestLook: async (look) => {
          chatLookRequests.push(look);
          return { ok: true };
        },
        saveProfile: async (change) => {
          savedUpdates.push(change);
          if (simulateUnsupported) {
            return { ok: false, unsupported: true, error: "501" };
          }
          return {
            ok: true,
            identity: {
              name: change.name === undefined || change.name === "" ? null : change.name,
              tone: null,
              look: change.look ?? "guard",
              revision: 3,
            },
          };
        },
      },
      workspace: null,
    };
    await act(async () => {
      root.render(
        React.createElement(AssistantActorProvider, {
          actor,
          children: React.createElement(AssistantAboutButton),
        }),
      );
    });
  };

  await renderAbout();
  const trigger = host.querySelector<HTMLButtonElement>(".assistant-about-trigger");
  assert(trigger !== null, "About trigger rendered");
  await act(async () => {
    trigger.click();
  });

  const dialog = document.body.querySelector<HTMLElement>("[role='dialog']");
  assert(dialog !== null, "About dialog opened");
  const nameInput = dialog.querySelector<HTMLInputElement>("input.field");
  assert(nameInput !== null && nameInput.value === "Nova", "About dialog pre-fills committed personal name");

  // Direct save on 200
  const designerBtn = dialog.querySelector<HTMLButtonElement>('[data-assistant-look="designer"]');
  assert(designerBtn !== null, "designer swatch in About dialog");
  await act(async () => {
    designerBtn.click();
  });
  const saveBtn = dialog.querySelector<HTMLButtonElement>('button[type="submit"]');
  assert(saveBtn !== null && !saveBtn.disabled, "Save identity enabled on avatar change");
  await act(async () => {
    saveBtn.click();
  });
  assert(
    savedUpdates.length === 1 && savedUpdates[0]?.expected_revision === 2 && savedUpdates[0]?.look === "designer",
    "hosted About dialog saves directly via saveProfile",
  );
  assert(chatLookRequests.join(",") === "", "direct save burns zero chat turns");

  // Older board 501 fallback -> forwards look change to requestLook and switches cleanly
  simulateUnsupported = true;
  const builderBtn = dialog.querySelector<HTMLButtonElement>('[data-assistant-look="builder"]');
  assert(builderBtn !== null, "builder swatch in About dialog");
  await act(async () => {
    builderBtn.click();
  });
  const saveBtn2 = dialog.querySelector<HTMLButtonElement>('button[type="submit"]');
  assert(saveBtn2 !== null, "Save identity button present before fallback");
  await act(async () => {
    saveBtn2.click();
  });
  assert(
    chatLookRequests.join(",") === "builder",
    "501 unsupported response degrades cleanly to chat avatar request",
  );

  await act(async () => {
    root.unmount();
  });
  console.log("CAD-2269 personal assistant bootstrap and hosted editor passed");
}

void main();
