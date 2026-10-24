import { createScreenReceiver, ScreenClosedError } from "@cadence/app-kit";

const root = document.querySelector("#root") ?? document.body;
const state = { connected: false, mode: "outlet", title: "App template", error: null, localDrawer: false };

function node(tag, props = {}, children = []) {
  const element = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (key === "class") element.className = value;
    else if (key === "text") element.textContent = value;
    else if (key === "onClick") element.addEventListener("click", value);
    else element.setAttribute(key, value);
  }
  for (const child of children) element.append(child);
  return element;
}

function render() {
  root.replaceChildren();
  root.dataset.presentation = state.mode;
  root.append(
    node("header", { class: "app-head" }, [
      node("div", {}, [
        node("p", { class: "eyebrow", text: "Cadence app" }),
        node("h1", { text: state.title }),
      ]),
      node("span", {
        class: `status ${state.connected ? "ok" : ""}`,
        text: state.connected ? "Host connected" : "Preview (no host)",
      }),
    ]),
  );
  const body = node("section", { class: "card" }, [
    node("p", {
      text: "This surface is app-owned. The frame, board drawer, scrim and close affordance are host-owned.",
    }),
    node("p", { class: "muted", text: `Presentation: ${state.mode}` }),
    node("button", { class: "btn", type: "button", onClick: openSettings, text: "Open settings drawer" }),
  ]);
  if (state.error) body.append(node("p", { class: "error", text: state.error }));
  root.append(body);
  if (state.localDrawer) {
    root.append(
      node("aside", { class: "local-drawer" }, [
        node("div", { class: "drawer-head" }, [
          node("h2", { text: "Settings" }),
          node("button", { class: "btn ghost", type: "button", onClick: closeSettings, text: "Close" }),
        ]),
        node("p", {
          text: "This is the app-local fallback. On a presenting host, app-kit opens the board-wide drawer instead.",
        }),
      ]),
    );
  }
}

async function openSettings() {
  try {
    await receiver.openDrawer("settings");
  } catch (error) {
    if (error instanceof ScreenClosedError) {
      state.localDrawer = true;
      state.error = null;
      render();
    } else {
      state.error = String(error?.message ?? error);
      render();
    }
  }
}

async function closeSettings() {
  state.localDrawer = false;
  try {
    await receiver.closeDrawer();
  } catch (error) {
    if (!(error instanceof ScreenClosedError)) state.error = String(error?.message ?? error);
  }
  render();
}

const receiver = createScreenReceiver({
  accepts: ["screen.v2", "screen-actions.v1", "screen-presentation.v1"],
  onPush(push) {
    state.connected = true;
    state.title = String(push?.title ?? "Host connected");
    state.error = null;
    render();
  },
  onPresentation(mode) {
    state.mode = mode;
    render();
  },
  onError(reason) {
    state.error = reason;
    render();
  },
});

render();
