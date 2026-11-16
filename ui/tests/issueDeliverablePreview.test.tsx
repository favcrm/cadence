import type * as EvidenceModule from "../src/features/issues/Evidence";
import type * as SessionKeyModule from "../src/lib/sessionKey";
import type * as PrivateSessionModule from "../src/lib/privateSession";
export {};

declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/issues/CAD-2277" });
for (const name of [
  "window",
  "document",
  "Node",
  "Element",
  "HTMLElement",
  "SVGElement",
  "navigator",
  "MutationObserver",
  "Event",
  "MouseEvent",
  "location",
  "history",
  "FileReader",
  "sessionStorage",
]) {
  Object.defineProperty(globalThis, name, {
    value: name === "window" ? win : win[name],
    configurable: true,
    writable: true,
  });
}
for (const name of ["addEventListener", "removeEventListener"]) {
  Object.defineProperty(globalThis, name, {
    value: win[name].bind(win),
    configurable: true,
  });
}
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module"),
  originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons")
    return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");

function assert(ok: unknown, msg: string): asserts ok {
  if (!ok) throw new Error(msg);
}

const flush = () =>
  React.act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
    await new Promise((resolve) => setTimeout(resolve, 0));
  });

const click = (el: Element | null) =>
  React.act(async () => {
    assert(el instanceof HTMLElement, "Expected clickable HTMLElement");
    el.click();
  });

const host = document.createElement("div");
document.body.appendChild(host);
const root = createRoot(host);

async function run() {
  const {
    Evidence,
    deliverablePreviewKind,
    deliverableDisplayName,
    rasterMimeFromBytes,
  } = require("../src/features/issues/Evidence") as typeof EvidenceModule;
  const { setSessionKey } = require("../src/lib/sessionKey") as typeof SessionKeyModule;
  const {
    allowPrivateReads,
    pausePrivateReads,
  } = require("../src/lib/privateSession") as typeof PrivateSessionModule;

  assert(
    deliverablePreviewKind("hosted:postmortem.html") === "html",
    "HTML deliverables resolve to html preview kind",
  );
  assert(
    deliverablePreviewKind("hosted:Customer-Recovery-Deck--art-7460ce3f47a4471391033ccad49faacd-v2.pdf") ===
      "pdf",
    "Linked PDF deliverables resolve to pdf preview kind",
  );
  assert(
    deliverablePreviewKind("summary.markdown") === "md",
    "Markdown deliverables resolve to md preview kind",
  );
  assert(
    deliverablePreviewKind("metrics.csv") === "csv",
    "CSV deliverables resolve to csv preview kind",
  );
  assert(
    deliverablePreviewKind("archive.zip") === null,
    "Opaque binary archives stay download-only",
  );
  assert(
    deliverableDisplayName(
      "hosted:Customer-Recovery-Deck--art-7460ce3f47a4471391033ccad49faacd-v2.pdf",
    ) === "Customer-Recovery-Deck (v2).pdf",
    "Linked workspace deliverables format a readable versioned label",
  );
  assert(
    deliverableDisplayName("hosted:Deck--art-short123-v1.pdf") === "Deck (v1).pdf",
    "Shorter valid art- IDs also format cleanly",
  );

  const pngBytes: Uint8Array<ArrayBuffer> = new Uint8Array([
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d,
  ]);
  const svgPolyglotBytes: Uint8Array<ArrayBuffer> = new Uint8Array(
    new TextEncoder().encode("<svg onload=alert(1)></svg>"),
  );
  assert(
    rasterMimeFromBytes(pngBytes) === "image/png",
    "PNG magic bytes resolve to image/png",
  );
  assert(
    rasterMimeFromBytes(svgPolyglotBytes) === null,
    "SVG polyglot bytes never sniff as raster image",
  );

  const originalFetch = globalThis.fetch;
  const originalCreate = URL.createObjectURL;
  const originalRevoke = URL.revokeObjectURL;
  let counter = 0;
  URL.createObjectURL = () => `blob:http://localhost/cad2277-${++counter}`;
  URL.revokeObjectURL = () => {};

  const payloads: Record<string, string | Uint8Array<ArrayBuffer>> = {
    "/api/issues/CAD-2277/artifacts/hosted%3Ascreenshot--art-7460ce3f47a4471391033ccad49faacd-v1.png":
      pngBytes,
    "/api/issues/CAD-2277/artifacts/hosted%3Apolyglot.png": svgPolyglotBytes,
    "/api/issues/CAD-2277/artifacts/hosted%3Apostmortem.html":
      "<h1>Incident Postmortem</h1><p>Containment verified.</p>",
    "/api/issues/CAD-2277/artifacts/hosted%3ACustomer-Recovery-Deck--art-7460ce3f47a4471391033ccad49faacd-v2.pdf":
      "%PDF-1.7\n%%EOF\n",
    "/api/issues/CAD-2277/artifacts/summary.md": "## Timeline\n- Contained in 12m",
    "/api/issues/CAD-2277/artifacts/bundle.zip": "PK\u0003\u0004",
  };

  globalThis.fetch = async (input) => {
    const path = String(input);
    const body = payloads[path];
    if (body === undefined) return new Response(null, { status: 404 });
    const response = new Response(body, {
      status: 200,
      headers: { "Content-Type": "application/octet-stream" },
    });
    // happy-dom's FileReader consumes its own Blob implementation.
    response.blob = async () =>
      new win.Blob([body], { type: "application/octet-stream" }) as Blob;
    return response;
  };

  allowPrivateReads();
  setSessionKey("cad2277-session");

  try {
    await React.act(async () => {
      root.render(
        React.createElement(Evidence, {
          id: "CAD-2277",
          images: [
            {
              name: "hosted:screenshot--art-7460ce3f47a4471391033ccad49faacd-v1.png",
              size: pngBytes.length,
            },
            { name: "hosted:polyglot.png", size: svgPolyglotBytes.length },
          ],
          files: [
            { name: "hosted:postmortem.html", size: 58 },
            {
              name: "hosted:Customer-Recovery-Deck--art-7460ce3f47a4471391033ccad49faacd-v2.pdf",
              size: 16,
            },
            { name: "summary.md", size: 31 },
            { name: "bundle.zip", size: 4 },
          ],
          readOnly: true,
        }),
      );
    });
    await flush();

    // Hosted raster PNG served as application/octet-stream renders inline img, while SVG polyglot renamed .png stays inert download.
    const renderedImg = host.querySelector('img[alt="screenshot (v1).png"]');
    assert(
      renderedImg instanceof HTMLElement &&
        renderedImg.getAttribute("src")?.startsWith("data:image/png;"),
      "Hosted octet-stream PNG renders inline data:image/png preview",
    );
    assert(
      host.querySelector('img[alt="polyglot.png"]') === null &&
        host.querySelector('a[download="polyglot.png"]') !== null,
      "Non-raster bytes served as octet-stream .png stay download-only",
    );
    assert(
      host.querySelector('a[download="Customer-Recovery-Deck (v2).pdf"]')?.textContent ===
        "Customer-Recovery-Deck (v2).pdf",
      "Linked deliverable row displays and downloads formatted displayName",
    );

    // Non-previewable files keep download without Preview button.
    assert(
      host.querySelector('a[download="bundle.zip"]') !== null,
      "Opaque archive keeps authenticated download link",
    );
    assert(
      host.querySelector('button[aria-label="Preview bundle.zip"]') === null,
      "Opaque archive does not advertise inline preview",
    );

    // Open HTML inline preview and verify strict iframe sandbox + source toggle.
    const htmlPreviewBtn = host.querySelector(
      'button[aria-label="Preview postmortem.html"]',
    );
    assert(htmlPreviewBtn !== null, "HTML deliverable exposes Preview control");
    await click(htmlPreviewBtn);
    await flush();

    const frame = host.querySelector('iframe[data-artifact-view="html"]');
    assert(frame !== null, "HTML deliverable renders inside sandboxed iframe");
    assert(
      frame.getAttribute("sandbox") === "",
      "HTML preview iframe enforces empty sandbox (opaque origin, no scripts)",
    );
    assert(
      frame.getAttribute("srcdoc")?.includes("script-src 'none'") &&
        frame.getAttribute("srcdoc")?.includes("Incident Postmortem"),
      "HTML preview srcDoc injects strict CSP and deliverable markup",
    );

    const sourceToggle = Array.from(
      host.querySelectorAll(".issue-deliverable-preview .vers button"),
    ).find((btn) => btn.textContent === "HTML") as HTMLElement | undefined;
    assert(sourceToggle !== undefined, "HTML preview offers source toggle");
    await click(sourceToggle);
    await flush();
    assert(
      host.querySelector('[data-artifact-view="html-source"]')?.textContent?.includes(
        "<h1>Incident Postmortem</h1>",
      ),
      "Source toggle displays raw HTML source safely",
    );

    // Open PDF preview and verify Open in new tab control.
    const pdfPreviewBtn = host.querySelector(
      'button[aria-label="Preview Customer-Recovery-Deck (v2).pdf"]',
    );
    assert(pdfPreviewBtn !== null, "Linked PDF deliverable exposes Preview control");
    await click(pdfPreviewBtn);
    await flush();
    assert(
      host.querySelector('[data-artifact-view="pdf"]') !== null,
      "PDF deliverable mounts PDF preview card",
    );

    // Open Markdown preview.
    const mdPreviewBtn = host.querySelector(
      'button[aria-label="Preview summary.md"]',
    );
    await click(mdPreviewBtn);
    await flush();
    assert(
      host.querySelector('[data-artifact-view="md"]')?.textContent?.includes(
        "Contained in 12m",
      ),
      "Markdown deliverable renders inline via Md",
    );

    // Pausing private reads immediately tears down open previews.
    await React.act(async () => {
      pausePrivateReads();
    });
    await flush();
    assert(
      host.querySelector(".issue-deliverable-preview") === null,
      "Pausing private reads closes open deliverable previews and drops bytes",
    );
  } finally {
    allowPrivateReads();
    await React.act(async () => root.render(null));
    globalThis.fetch = originalFetch;
    URL.createObjectURL = originalCreate;
    URL.revokeObjectURL = originalRevoke;
    setSessionKey(null);
  }

  await React.act(async () => root.unmount());
  await win.happyDOM.close();
  console.log("issueDeliverablePreview: passed");
}

run().catch(async (err) => {
  await React.act(async () => root.unmount());
  await win.happyDOM.close();
  console.error(err);
  throw err;
});
