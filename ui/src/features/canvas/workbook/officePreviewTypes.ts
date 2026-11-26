import type { DocxDocument } from "./vendor/docx-document";
import type { SlideDeck } from "./vendor/pptx-slides";
import type { WorkbookSheets } from "./vendor/xlsx-sheets";

export type OfficeFormat = "docx" | "pptx" | "xlsx";

export const OFFICE_FORMATS: Readonly<Record<string, OfficeFormat | undefined>> = {
  "application/vnd.openxmlformats-officedocument.wordprocessingml.document": "docx",
  "application/vnd.openxmlformats-officedocument.presentationml.presentation": "pptx",
  "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet": "xlsx",
};

export interface OfficePreviewRequest {
  format: OfficeFormat;
  bytes: ArrayBuffer;
}

export type OfficePreviewResult =
  | { format: "docx"; doc: DocxDocument }
  | { format: "pptx"; deck: SlideDeck }
  | { format: "xlsx"; workbook: WorkbookSheets }
  | { format: "error" };
