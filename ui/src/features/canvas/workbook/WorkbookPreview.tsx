import { useId, useState, type CSSProperties } from "react";
import { useLocaleTag } from "../../../lib/locale";
import type { SheetCell, SheetCellStyle, WorkbookSheets } from "./vendor/xlsx-sheets";

function columnName(index: number): string {
  let number = index + 1;
  let name = "";
  while (number > 0) { number--; name = String.fromCharCode(65 + number % 26) + name; number = Math.floor(number / 26); }
  return name;
}

function cellStyle(cell: SheetCell | undefined, style: SheetCellStyle | undefined): CSSProperties | undefined {
  if (!style) return undefined;
  const align = style.horizontal === "general" ? (cell?.kind === "number" || cell?.kind === "date" ? "right" : "left") : style.horizontal;
  return {
    color: cell?.color ?? style.font.color ?? "#111111",
    backgroundColor: style.fill ?? "#ffffff",
    fontWeight: style.font.bold ? "bold" : undefined,
    fontStyle: style.font.italic ? "italic" : undefined,
    textDecoration: [style.font.underline !== "none" ? "underline" : "", style.font.strike ? "line-through" : ""].filter(Boolean).join(" ") || undefined,
    textAlign: align === "right" || align === "center" || align === "justify" ? align : "left",
    whiteSpace: style.wrap ? "pre-wrap" : "pre",
  };
}

export default function WorkbookPreview({ workbook, title }: { workbook: WorkbookSheets; title: string }) {
  const zh = useLocaleTag() === "zh-TW";
  const id = useId();
  const [selected, setSelected] = useState(0);
  const sheet = workbook.sheets[selected];
  if (!sheet) return <div className="workbook-status">{zh ? "此活頁簿沒有可顯示的工作表。" : "This workbook has no visible worksheets."}</div>;
  const rows = new Map(sheet.rows.map((row) => [row.index, row]));
  const columns = new Map(sheet.columns.map((column) => [column.index, column]));
  const visibleColumns = Array.from({ length: sheet.columnCount }, (_, i) => i).filter((i) => !columns.get(i)?.hidden);
  const visibleRows = Array.from({ length: sheet.rowCount }, (_, i) => i).filter((i) => !rows.get(i)?.hidden);
  return <section className="workbook-preview" aria-label={title}>
    <div className="workbook-toolbar"><label htmlFor={id}>{zh ? "工作表" : "Worksheet"}</label><select id={id} value={selected} onChange={(event) => setSelected(Number(event.target.value))}>{workbook.sheets.map((item, index) => <option value={index} key={index}>{item.name || `${zh ? "工作表" : "Sheet"} ${index + 1}`}</option>)}</select></div>
    <p className="workbook-notice">{zh ? "唯讀預覽；公式顯示上次儲存的結果，不會重新計算。" : "Read-only preview; formulas show their last saved values and are not recalculated."}{(workbook.truncated || sheet.truncated) && (zh ? " 部分工作表或儲存格因預覽限制未顯示。" : " Some sheets or cells are omitted by preview limits.")}{sheet.uncalculatedFormulas && (zh ? " 部分公式沒有儲存結果。" : " Some formulas have no saved result.")}{sheet.undrawn.length > 0 && (zh ? " 圖表、圖片或其他物件未顯示。" : " Charts, images or other objects are not shown.")}</p>
    {visibleRows.length === 0 || visibleColumns.length === 0 ? <div className="workbook-status">{zh ? "此工作表是空白的。" : "This worksheet is empty."}</div> : <div className="workbook-scroll" tabIndex={0} role="region" aria-label={zh ? "工作表內容" : "Worksheet contents"}>
      <table><caption className="workbook-sr-only">{sheet.name || title}</caption><thead><tr><th aria-label={zh ? "列" : "Row"} />{visibleColumns.map((i) => <th scope="col" key={i}>{columnName(i)}</th>)}</tr></thead><tbody>{visibleRows.map((index) => {
        const row = rows.get(index);
        const cells = new Map(row?.cells.map((cell) => [cell.column, cell]));
        return <tr key={index}><th scope="row">{index + 1}</th>{visibleColumns.map((column) => {
          const cell = cells.get(column);
          const style = workbook.styles[cell?.style ?? row?.style ?? columns.get(column)?.style ?? 0];
          return <td key={column} style={cellStyle(cell, style)}>{cell?.text ?? ""}</td>;
        })}</tr>;
      })}</tbody></table>
    </div>}
  </section>;
}
