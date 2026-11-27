import {
  SCREEN_FILE_DOWNLOAD_ACK_TYPE,
  SCREEN_FILE_DOWNLOAD_MAX_BYTES,
  SCREEN_FILE_DOWNLOAD_TYPE,
  SCREEN_PDF_EXPORT_ACK_TYPE,
  SCREEN_PDF_EXPORT_HTML_MAX_BYTES,
  SCREEN_PDF_EXPORT_TYPE,
  SCREEN_SUBMIT_DOCUMENT_ACK_TYPE,
  SCREEN_SUBMIT_DOCUMENT_MAX_BYTES,
  SCREEN_SUBMIT_DOCUMENT_TYPE,
  consumeWorksImportFile,
  handleScreenFileDownloadMessage,
  handleScreenPdfExportMessage,
  handleScreenSubmitDocumentMessage,
  parseScreenFileDownload,
  parseScreenPdfExport,
  parseScreenSubmitDocument,
  stageWorksImportFile,
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

  // 7. parseScreenSubmitDocument & handleScreenSubmitDocumentMessage
  const validPdfSubmit = parseScreenSubmitDocument({
    type: SCREEN_SUBMIT_DOCUMENT_TYPE,
    requestId: "sub_pdf_1",
    title: "Q4 Report",
    label: "report",
    kind: "pdf",
    html: validHtml,
    paper: "a4",
    submit: true,
  });
  assert(validPdfSubmit !== null && validPdfSubmit.kind === "pdf" && validPdfSubmit.submit === true, "valid pdf submit parses");

  const pkZip = new Uint8Array([0x50, 0x4b, 0x03, 0x04, 0x14, 0x00]).buffer;
  const validDocxSubmit = parseScreenSubmitDocument({
    type: SCREEN_SUBMIT_DOCUMENT_TYPE,
    requestId: "sub_docx_1",
    title: "合約草案",
    label: "proposal",
    kind: "docx",
    bytes: pkZip,
    submit: false,
  });
  assert(validDocxSubmit !== null && validDocxSubmit.kind === "docx" && validDocxSubmit.submit === false, "valid docx submit parses");

  assert(
    parseScreenSubmitDocument({
      type: SCREEN_SUBMIT_DOCUMENT_TYPE,
      requestId: "sub_bad_magic",
      title: "Fake",
      label: "report",
      kind: "xlsx",
      bytes: new Uint8Array([1, 2, 3, 4]).buffer,
      submit: true,
    }) === null,
    "reject non-ZIP Office bytes",
  );
  assert(
    parseScreenSubmitDocument({
      type: SCREEN_SUBMIT_DOCUMENT_TYPE,
      requestId: "sub_bad_label",
      title: "Bad Label",
      label: "a/b",
      kind: "pdf",
      html: validHtml,
      submit: false,
    }) === null,
    "reject slash in label",
  );
  assert(
    parseScreenSubmitDocument({
      type: SCREEN_SUBMIT_DOCUMENT_TYPE,
      requestId: "sub_too_big",
      title: "Big",
      label: "report",
      kind: "pptx",
      bytes: new Uint8Array(SCREEN_SUBMIT_DOCUMENT_MAX_BYTES + 1).buffer,
      submit: true,
    }) === null,
    "reject >10 MiB Office payload",
  );

  const subReplies: unknown[] = [];
  const subSource = { postMessage(msg: unknown) { subReplies.push(msg); } };
  assert(
    handleScreenSubmitDocumentMessage(
      {
        source: subSource as unknown as MessageEventSource,
        data: {
          type: SCREEN_SUBMIT_DOCUMENT_TYPE,
          requestId: "sub_ok",
          title: "Deck",
          label: "proposal",
          kind: "pptx",
          bytes: pkZip,
          submit: true,
        },
      },
      subSource,
      () => true,
      async () => ({ id: "doc_123", stage: "in_review" }),
    ) === true,
    "handled valid submit document message",
  );
  await new Promise((r) => setTimeout(r, 0));
  assert(
    (subReplies.length as number) === 1 &&
      (subReplies[0] as { type?: string; ok?: boolean; id?: string; stage?: string }).type === SCREEN_SUBMIT_DOCUMENT_ACK_TYPE &&
      (subReplies[0] as { ok?: boolean }).ok === true &&
      (subReplies[0] as { id?: string }).id === "doc_123",
    "expected cadence-submit-document-ack ok:true",
  );

  // Wrong source must be ignored without replying
  handleScreenSubmitDocumentMessage(
    {
      source: {} as MessageEventSource,
      data: { type: SCREEN_SUBMIT_DOCUMENT_TYPE, requestId: "sub_evil", title: "X", label: "report", kind: "pptx", bytes: pkZip, submit: true },
    },
    subSource,
    () => true,
    async () => { throw new Error("must not run"); },
  );
  await new Promise((r) => setTimeout(r, 0));
  assert((subReplies.length as number) === 1, "wrong source must not receive submit ack");

  // 8. stageWorksImportFile / consumeWorksImportFile single-use lifecycle
  const impId = stageWorksImportFile({
    installId: "inst_works",
    fileName: "報價單.xlsx",
    mimeType: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    bytes: pkZip,
  });
  const consumed = consumeWorksImportFile("inst_works");
  assert(consumed !== null && consumed.id === impId && consumed.fileName === "報價單.xlsx", "staged import consumed");
  assert(consumeWorksImportFile("inst_works") === null, "second consume returns null");
})();

console.log("screenFileDownload tests passed");
