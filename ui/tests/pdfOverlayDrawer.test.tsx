declare function require(name: string): any;
export {};

function ok(cond: unknown, why: string): void {
  if (!cond) throw new Error(`failed: ${why}`);
}
function equal<T>(a: T, e: T, why: string): void {
  if (JSON.stringify(a) !== JSON.stringify(e)) {
    throw new Error(`${why}: expected ${JSON.stringify(e)}, got ${JSON.stringify(a)}`);
  }
}

const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const name of [
  "window",
  "document",
  "Node",
  "Element",
  "HTMLElement",
  "HTMLButtonElement",
  "navigator",
  "Event",
  "MouseEvent",
  "KeyboardEvent",
  "location",
]) {
  Object.defineProperty(globalThis, name, {
    value: name === "window" ? win : win[name],
    configurable: true,
    writable: true,
  });
}
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const { LocaleTextProvider } = require("../src/lib/localeText") as typeof import("../src/lib/localeText");
const canvasStore = require("../src/features/canvas/canvasStore") as typeof import("../src/features/canvas/canvasStore");
const PdfOverlayDrawer = (require("../src/features/canvas/PdfOverlayDrawer") as typeof import("../src/features/canvas/PdfOverlayDrawer")).default;

// 1. canvasStore pdfOverlay lifecycle
canvasStore.resetCanvas("c-pdf", "k", "app");
canvasStore.openArtifact({ id: "art-1", version: 1, kind: "html", title: "Spec", bytes: 120 });
const sampleBytes = new TextEncoder().encode("%PDF-1.4\n%%EOF\n").buffer;
canvasStore.openPdfOverlay({ title: "Invoice.pdf", bytes: sampleBytes, mime: "application/pdf", fileName: "Invoice.pdf" });
equal(canvasStore.canvasSnapshot().pdfOverlay?.title, "Invoice.pdf", "openPdfOverlay stores active overlay");
canvasStore.closePdfOverlay();
equal(canvasStore.canvasSnapshot().pdfOverlay, null, "closePdfOverlay clears active overlay");
canvasStore.openPdfOverlay({ title: "Quote.pdf", bytes: sampleBytes, mime: "application/pdf" });
canvasStore.closeDrawer();
equal([canvasStore.canvasSnapshot().open, canvasStore.canvasSnapshot().pdfOverlay], [false, null], "closeDrawer clears pdfOverlay");

// 2. PdfOverlayDrawer interaction: wide -> full canvas width toggle, Escape, backdrop, focus restore, zh-TW
const opener = document.createElement("button");
opener.textContent = "Open PDF";
document.body.appendChild(opener);
opener.focus();

const host = document.createElement("div");
document.body.appendChild(host);
const root = createRoot(host);
let closedCount = 0;

React.act(() => {
  root.render(
    React.createElement(
      LocaleTextProvider,
      { value: (s: string) => (s === "Full width" ? "展開至全寬" : s === "Default width" ? "預設寬度" : s) },
      React.createElement(PdfOverlayDrawer, {
        title: "Quotation Q4.pdf",
        bytes: sampleBytes,
        fileName: "Quotation Q4.pdf",
        scope: "canvas",
        onClose: () => {
          closedCount += 1;
        },
      }),
    ),
  );
});

const dialog = host.querySelector<HTMLElement>('[role="dialog"]');
ok(dialog, "dialog renders");
equal(dialog!.getAttribute("data-full"), null, "starts at wide default width");
const widthBtn = host.querySelector<HTMLButtonElement>(".cv-pdf-width-btn");
ok(widthBtn, "width toggle button exists");
equal(widthBtn!.textContent?.trim(), "展開至全寬", "uses zh-TW translation for Full width");
equal(widthBtn!.getAttribute("aria-pressed"), "false", "aria-pressed starts false");

React.act(() => {
  widthBtn!.click();
});
equal(dialog!.getAttribute("data-full"), "true", "toggles to full canvas width");
equal(widthBtn!.getAttribute("aria-pressed"), "true", "aria-pressed updates to true");
equal(widthBtn!.textContent?.trim(), "預設寬度", "updates toggle label in zh-TW");

// Escape triggers onClose without propagating out
let parentSawEscape = false;
const parentHandler = () => {
  parentSawEscape = true;
};
window.addEventListener("keydown", parentHandler);
React.act(() => {
  window.dispatchEvent(new win.KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
});
window.removeEventListener("keydown", parentHandler);
equal(closedCount, 1, "Escape invokes onClose");
equal(parentSawEscape, false, "Escape stops propagation so CanvasDrawer stays open");

// Backdrop click triggers onClose
const backdrop = host.querySelector<HTMLElement>(".cv-pdf-backdrop");
ok(backdrop, "backdrop renders");
React.act(() => {
  backdrop!.click();
});
equal(closedCount, 2, "backdrop click invokes onClose");

// Unmount restores focus to previous opener
React.act(() => {
  root.unmount();
});
ok(document.activeElement === opener, "closing restores focus to triggering element");
opener.remove();
host.remove();
