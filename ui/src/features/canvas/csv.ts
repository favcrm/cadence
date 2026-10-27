/**
 * CAD-1728: a small RFC 4180 reader for the canvas's CSV view. Quoted fields
 * may hold commas, doubled quotes and line breaks. Anything malformed (an
 * unterminated quote, text after a closing quote) returns null so the view
 * falls back to plain text.
 */
export const CSV_MAX_ROWS = 2000;
export const CSV_MAX_COLS = 50;

export interface CsvTable {
  header: string[];
  rows: string[][];
  /** Data rows in the file (the table shows at most `CSV_MAX_ROWS`). */
  total: number;
  /** Columns in the widest record (the table shows at most `CSV_MAX_COLS`). */
  columns: number;
}

export function parseCsv(text: string): CsvTable | null {
  const records: string[][] = [];
  let row: string[] = [];
  let field = "";
  let quoted = false;
  let wasQuoted = false;
  // A quoted field makes its record real even when the field is empty (`""`).
  let rowQuoted = false;
  let i = text.charCodeAt(0) === 0xfeff ? 1 : 0;
  const endField = () => {
    row.push(field);
    field = "";
    wasQuoted = false;
  };
  const endRow = () => {
    endField();
    if (row.length > 1 || row[0] !== "" || rowQuoted) records.push(row);
    row = [];
    rowQuoted = false;
  };
  for (; i < text.length; i++) {
    const c = text[i];
    if (quoted) {
      if (c !== '"') field += c;
      else if (text[i + 1] === '"') {
        field += '"';
        i++;
      } else quoted = false;
      continue;
    }
    if (c === '"') {
      if (field !== "" || wasQuoted) return null;
      quoted = wasQuoted = rowQuoted = true;
    } else if (wasQuoted && c !== "," && c !== "\n" && c !== "\r") {
      return null;
    } else if (c === ",") endField();
    else if (c === "\n") endRow();
    else if (c === "\r") {
      if (text[i + 1] === "\n") i++;
      endRow();
    } else field += c;
  }
  if (quoted) return null;
  if (field !== "" || wasQuoted || row.length > 0) endRow();
  if (records.length === 0) return null;
  const [header, ...rest] = records;
  const columns = records.reduce((n, r) => Math.max(n, r.length), 0);
  const cap = (r: string[]) => (r.length > CSV_MAX_COLS ? r.slice(0, CSV_MAX_COLS) : r);
  return { header: cap(header), rows: rest.slice(0, CSV_MAX_ROWS).map(cap), total: rest.length, columns };
}
