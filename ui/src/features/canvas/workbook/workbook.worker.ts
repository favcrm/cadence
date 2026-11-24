import { readWorkbookSheets } from "./vendor/xlsx-sheets";

self.onmessage = async (event: MessageEvent<ArrayBuffer>) => {
  try {
    const workbook = await readWorkbookSheets(new Uint8Array(event.data), {
      maxSheets: 10,
      maxRows: 200,
      maxColumns: 40,
    });
    self.postMessage({ workbook });
  } catch {
    self.postMessage({ error: true });
  }
};
