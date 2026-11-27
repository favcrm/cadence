import {
  SCREEN_FILE_DOWNLOAD_ACK_TYPE,
  SCREEN_FILE_DOWNLOAD_MAX_BYTES,
  SCREEN_FILE_DOWNLOAD_TYPE,
  SCREEN_PDF_EXPORT_ACK_TYPE,
  SCREEN_PDF_EXPORT_HTML_MAX_BYTES,
  SCREEN_PDF_EXPORT_TYPE,
  handleScreenFileDownloadMessage,
  handleScreenPdfExportMessage,
  parseScreenFileDownload,
  parseScreenPdfExport,
} from "../src/features/workspace-apps/screen/screenFileDownload";

export {};

function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}

// 1. All six allowlisted formats pass and normalize to canonical MIME types.
{
  const cases: Array<[string, string, string]> = [
    ["notes.md", "text/markdown", "text/markdown;charset=utf-8"],
    ["deck.html", "text/html;charset=utf-8", "text/html;charset=utf-8"],
    ["sheet.csv", "text/csv", "text/csv;charset=utf-8"],
    [
      "budget.xlsx",
      "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
      "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ],
    [
      "slides.pptx",
      "application/vnd.openxmlformats-officedocument.presentationml.presentation",
      "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ],
    [
      "spec.docx",
      "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
      "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ],
    [
      "summary.pdf",
      "application/pdf",
      "application/pdf",
    ],
  ];

  for (const [filename, mimeType, expectedMime] of cases) {
    const parsed = parseScreenFileDownload({
      type: SCREEN_FILE_DOWNLOAD_TYPE,
      requestId: "req_1",
      filename,
      mimeType,
      bytes: new Uint8Array([1, 2, 3]).buffer,
    });
    assert(parsed !== null, `expected valid download for ${filename}`);
    assert(parsed.fileName === filename, `expected basename ${filename}`);
    assert(parsed.mimeType === expectedMime, `expected canonical MIME for ${filename}`);
    assert(parsed.bytes.byteLength === 3, `expected 3 bytes for ${filename}`);
  }
}

// 2. Refuse disallowed extensions, path traversal, MIME mismatch, empty, and >16 MiB payloads.
{
  const buf = new Uint8Array([1, 2, 3]).buffer;

  for (const badName of [
    "evil.js",
    "bad.svg",
    "run.exe",
    "script.sh",
    "archive.zip",
    "a.constructor",
    "a.toString",
    "a.__proto__",
    "a.valueOf",
    "../secret.xlsx",
    "a/b.docx",
    "a\\b.docx",
    "noext",
    ".hidden",
    "bad\nname.md",
  ]) {
    const res = parseScreenFileDownload({
      type: SCREEN_FILE_DOWNLOAD_TYPE,
      requestId: "req_bad",
      filename: badName,
      mimeType: "text/markdown",
      bytes: buf,
    });
    assert(res === null, `should refuse ${badName}`);
  }

  assert(
    parseScreenFileDownload({
      type: SCREEN_FILE_DOWNLOAD_TYPE,
      requestId: "req_mime",
      filename: "sheet.xlsx",
      mimeType: "text/html",
      bytes: buf,
    }) === null,
    "should refuse MIME/extension mismatch",
  );

  assert(
    parseScreenFileDownload({
      type: SCREEN_FILE_DOWNLOAD_TYPE,
      requestId: "req_empty",
      filename: "doc.md",
      mimeType: "text/markdown",
      bytes: new ArrayBuffer(0),
    }) === null,
    "should refuse empty buffer",
  );

  assert(
    parseScreenFileDownload({
      type: SCREEN_FILE_DOWNLOAD_TYPE,
      requestId: "req_big",
      filename: "doc.md",
      mimeType: "text/markdown",
      bytes: new ArrayBuffer(SCREEN_FILE_DOWNLOAD_MAX_BYTES + 1),
    }) === null,
    "should refuse oversized buffer",
  );
}

// 3. Verify exact iframe source, active flag, and exact ack reply.
{
  const saves: Array<{ fileName: string; mimeType: string; size: number }> = [];
  const replies: unknown[] = [];
  const expectedSource = {
    postMessage(msg: unknown) {
      replies.push(msg);
    },
  };
  const otherSource = { postMessage() {} };
  const payload = {
    type: SCREEN_FILE_DOWNLOAD_TYPE,
    requestId: "req_42",
    filename: "report.docx",
    mimeType: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    bytes: new Uint8Array([80, 75, 3, 4]).buffer,
  };

  assert(
    handleScreenFileDownloadMessage(
      { source: expectedSource as unknown as MessageEventSource, data: { type: "other" } },
      expectedSource,
      () => true,
      () => {},
    ) === false,
    "unrelated message must return false",
  );

  assert(
    handleScreenFileDownloadMessage(
      { source: otherSource as unknown as MessageEventSource, data: payload },
      expectedSource,
      () => true,
      (b, m, f) => saves.push({ fileName: f, mimeType: m, size: b.byteLength }),
    ) === true,
    "spoofed source must be intercepted",
  );
  assert(saves.length === 0 && replies.length === 0, "spoofed source must neither save nor ack");

  assert(
    handleScreenFileDownloadMessage(
      { source: expectedSource as unknown as MessageEventSource, data: payload },
      expectedSource,
      () => false,
      (b, m, f) => saves.push({ fileName: f, mimeType: m, size: b.byteLength }),
    ) === true,
    "inactive frame must be intercepted",
  );
  assert(saves.length === 0 && replies.length === 0, "inactive frame must neither save nor ack");

  assert(
    handleScreenFileDownloadMessage(
      { source: expectedSource as unknown as MessageEventSource, data: payload },
      expectedSource,
      () => true,
      (b, m, f) => saves.push({ fileName: f, mimeType: m, size: b.byteLength }),
    ) === true,
    "valid frame download must be handled",
  );
  assert((saves.length as number) === 1 && saves[0].fileName === "report.docx" && saves[0].size === 4, "expected saved file");
  assert(
    (replies.length as number) === 1 &&
      (replies[0] as { type?: string; requestId?: string }).type === SCREEN_FILE_DOWNLOAD_ACK_TYPE &&
      (replies[0] as { requestId?: string }).requestId === "req_42",
    "expected cadence-file-download-ack",
  );
}

// 4. PDF export validation, source binding, and async ack replies.
void (async () => {
  const validHtml = "<!doctype html><html><body><p>Report</p></body></html>";
  const parsed = parseScreenPdfExport({
    type: SCREEN_PDF_EXPORT_TYPE,
    requestId: "pdf_1",
    filename: "board-report.pdf",
    html: validHtml,
    paper: "a4",
  });
  assert(parsed !== null && parsed.fileName === "board-report.pdf" && parsed.paper === "a4", "expected valid pdf export request");

  assert(
    parseScreenPdfExport({
      type: SCREEN_PDF_EXPORT_TYPE,
      requestId: "pdf_bad_ext",
      filename: "board-report.html",
      html: validHtml,
    }) === null,
    "must require .pdf extension",
  );
  assert(
    parseScreenPdfExport({
      type: SCREEN_PDF_EXPORT_TYPE,
      requestId: "pdf_bad_paper",
      filename: "board-report.pdf",
      html: validHtml,
      paper: "a3",
    }) === null,
    "must validate paper enum",
  );
  assert(
    parseScreenPdfExport({
      type: SCREEN_PDF_EXPORT_TYPE,
      requestId: "pdf_big",
      filename: "board-report.pdf",
      html: "x".repeat(SCREEN_PDF_EXPORT_HTML_MAX_BYTES + 1),
    }) === null,
    "must enforce html byte limit",
  );

  const replies: unknown[] = [];
  const expectedSource = {
    postMessage(msg: unknown) {
      replies.push(msg);
    },
  };
  const otherSource = { postMessage() {} };
  let active = true;
  let calls = 0;

  assert(
    handleScreenPdfExportMessage(
      {
        source: otherSource as unknown as MessageEventSource,
        data: { type: SCREEN_PDF_EXPORT_TYPE, requestId: "pdf_spoof", filename: "a.pdf", html: validHtml },
      },
      expectedSource,
      () => active,
      async () => { calls += 1; },
    ) === true,
    "spoofed pdf source must be intercepted",
  );
  assert(calls === 0 && replies.length === 0, "spoofed pdf request must not run");

  assert(
    handleScreenPdfExportMessage(
      {
        source: expectedSource as unknown as MessageEventSource,
        data: { type: SCREEN_PDF_EXPORT_TYPE, requestId: "pdf_ok", filename: "a.pdf", html: validHtml },
      },
      expectedSource,
      () => active,
      async () => { calls += 1; },
    ) === true,
    "valid pdf request must run",
  );
  await new Promise((r) => setTimeout(r, 0));
  assert(
    (calls as number) === 1 &&
      (replies.length as number) === 1 &&
      (replies[0] as { type?: string; requestId?: string; ok?: boolean }).type === SCREEN_PDF_EXPORT_ACK_TYPE &&
      (replies[0] as { ok?: boolean }).ok === true,
    "expected cadence-pdf-export-ack ok:true",
  );

  assert(
    handleScreenPdfExportMessage(
      {
        source: expectedSource as unknown as MessageEventSource,
        data: { type: SCREEN_PDF_EXPORT_TYPE, requestId: "pdf_err", filename: "a.pdf", html: validHtml },
      },
      expectedSource,
      () => active,
      async () => { throw new Error("render boom"); },
    ) === true,
    "failing pdf request should still be handled",
  );
  await new Promise((r) => setTimeout(r, 0));
  assert(
    (replies.length as number) === 2 &&
      (replies[1] as { ok?: boolean; error?: string }).ok === false &&
      (replies[1] as { error?: string }).error === "render boom",
    "expected cadence-pdf-export-ack ok:false with error",
  );

  // Stale frame suppression after unmount
  handleScreenPdfExportMessage(
    {
      source: expectedSource as unknown as MessageEventSource,
      data: { type: SCREEN_PDF_EXPORT_TYPE, requestId: "pdf_stale", filename: "a.pdf", html: validHtml },
    },
    expectedSource,
    () => active,
    async () => { active = false; },
  );
  await new Promise((r) => setTimeout(r, 0));
  assert((replies.length as number) === 2, "stale frame must suppress reply");
})();

console.log("screenFileDownload tests passed");
