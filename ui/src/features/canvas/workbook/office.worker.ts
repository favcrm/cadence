import type { OfficePreviewRequest, OfficePreviewResult } from "./officePreviewTypes";
import { readDocxDocument } from "./vendor/docx-document";
import { readPptxSlides } from "./vendor/pptx-slides";
import { readWorkbookSheets } from "./vendor/xlsx-sheets";

self.onmessage = async (event: MessageEvent<OfficePreviewRequest>) => {
  const { format, bytes } = event.data;
  const source = new Uint8Array(bytes);
  let result: OfficePreviewResult;
  try {
    switch (format) {
      case "docx": result = { format, doc: await readDocxDocument(source, { maxBlocks: 1500 }) }; break;
      case "pptx": result = { format, deck: await readPptxSlides(source, { maxSlides: 50 }) }; break;
      case "xlsx":
        result = { format, workbook: await readWorkbookSheets(source, { maxSheets: 10, maxRows: 200, maxColumns: 40 }) };
        break;
    }
    self.postMessage(result);
  } catch {
    self.postMessage({ format: "error" } satisfies OfficePreviewResult);
  }
};
