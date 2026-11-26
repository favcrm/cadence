import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { HireBlock, WriteGate } from "../src/features/auth/WriteGate";
import { LocaleProvider } from "../src/lib/locale";
import { appViewId, appViewRef } from "../src/features/canvas/appView";
import {
  canvasSnapshot,
  openAppView,
  openAppViewEditable,
  resetCanvas,
  setAppViewEditable,
} from "../src/features/canvas/canvasStore";
import ViewHeader from "../src/features/canvas/ViewHeader";

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

const sampleRef = (over: Record<string, unknown> = {}) => ({
  installId: "install_works",
  tag: "works",
  contextId: "default",
  route: { section: "docs", view: "new" as const, record_id: null, segment_id: null },
  title: "New Doc",
  appLabel: "Works",
  href: "/apps/works?install=install_works&ctx=default&works=docs",
  ...over,
});

// 1. CAD-2471 fail-closed invariant: appViewRef strips any untrusted `editable` field.
const stripped = appViewRef(sampleRef({ editable: true }));
assert(stripped !== null && stripped.editable === undefined, "appViewRef must strip untrusted editable field");

// 2. openAppView always opens read-only; openAppViewEditable opts into editing; setAppViewEditable toggles cleanly.
resetCanvas("conv_1", "k", "app");
assert(openAppView(sampleRef({ editable: true })), "openAppView opens valid ref");
const id = appViewId(stripped!);
assert(canvasSnapshot().tabs[0]?.app?.editable === undefined, "openAppView stores read-only tab even if input carried editable");

assert(openAppViewEditable(sampleRef()), "openAppViewEditable enables edit mode");
assert(canvasSnapshot().tabs[0]?.app?.editable === true, "openAppViewEditable marks tab editable");

setAppViewEditable(id, false);
assert(canvasSnapshot().tabs[0]?.app?.editable === undefined, "setAppViewEditable(false) returns tab to read-only");

setAppViewEditable(id, true);
assert(canvasSnapshot().tabs[0]?.app?.editable === true, "setAppViewEditable(true) enables editing");

// Re-opening from a chat card resets editable back to read-only.
assert(openAppView(sampleRef()), "openAppView focuses existing tab");
assert(canvasSnapshot().tabs[0]?.app?.editable === undefined, "openAppView resets tab back to read-only");

// 3. ViewHeader exposes Edit here / Done editing and status badge accurately.
const roHtml = renderToStaticMarkup(
  createElement(
    LocaleProvider,
    null,
    createElement(ViewHeader, {
      mark: null,
      back: null,
      title: "Budget",
      appName: "Works",
      options: "",
      note: "Read-only here — open in Works to edit",
      editing: false,
      edit: { active: false, label: "Edit here", onToggle: () => {} },
      open: { label: "Open in Works", href: "/apps/works", onOpen: () => {} },
      expand: { expanded: false, enabled: true, onToggle: () => {} },
      onClose: () => {},
    }),
  ),
);
assert(roHtml.includes("Edit here") && roHtml.includes("Read-only"), "read-only header shows Edit here toggle and Read-only status");

const editHtml = renderToStaticMarkup(
  createElement(
    LocaleProvider,
    null,
    createElement(ViewHeader, {
      mark: null,
      back: null,
      title: "Budget",
      appName: "Works",
      options: "",
      note: "Read-only here — open in Works to edit",
      editing: true,
      edit: { active: true, label: "Done editing", onToggle: () => {} },
      open: { label: "Open in Works", href: "/apps/works", onOpen: () => {} },
      expand: { expanded: false, enabled: true, onToggle: () => {} },
      onClose: () => {},
    }),
  ),
);
assert(editHtml.includes("Done editing") && editHtml.includes("Editing") && !editHtml.includes("data-read-only"), "editing header shows Done editing and Editing status");
void WriteGate;
void HireBlock;
