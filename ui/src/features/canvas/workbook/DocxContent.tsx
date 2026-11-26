import type { CSSProperties } from "react";
import { useLocaleTag } from "../../../lib/locale";
import type { DocxBlock, DocxBoxBorders, DocxDocument, DocxRunProperties } from "./vendor/docx-document";
import "./office-preview.css";

function color(value: string | undefined): string | undefined {
  return value && /^#[\da-f]{6}$/i.test(value) ? value : undefined;
}

function runStyle(run: DocxRunProperties): CSSProperties {
  return {
    fontFamily: /^[\w ,'-]+$/.test(run.font) ? `"${run.font.replace(/"/g, "")}"` : undefined,
    fontSize: `${run.size}pt`, fontWeight: run.bold ? "bold" : undefined,
    fontStyle: run.italic ? "italic" : undefined, color: color(run.color),
    backgroundColor: color(run.highlight) ?? color(run.shading),
    textDecorationLine: [run.underline && "underline", run.strike && "line-through"].filter(Boolean).join(" ") || undefined,
    textDecorationColor: color(run.underlineColor),
    verticalAlign: run.verticalAlign === "superscript" ? "super" : run.verticalAlign === "subscript" ? "sub" : undefined,
    textTransform: run.caps ? "uppercase" : undefined, fontVariant: run.smallCaps ? "small-caps" : undefined,
    letterSpacing: `${run.letterSpacing}pt`,
  };
}

function borders(edges: DocxBoxBorders): CSSProperties {
  const edge = (side: DocxBoxBorders["top"]) => side ? `${side.width}pt ${side.style} ${color(side.color) ?? "#000000"}` : undefined;
  return { borderTop: edge(edges.top), borderRight: edge(edges.right), borderBottom: edge(edges.bottom), borderLeft: edge(edges.left) };
}

function Blocks({ blocks }: { blocks: readonly DocxBlock[] }) {
  return <>{blocks.map((block, index) => {
    if (block.kind === "pageBreak") return <hr className="office-page-break" key={index} />;
    if (block.kind === "table") {
      const width = block.columnWidths.reduce((sum, column) => sum + column, 0);
      return <div className="office-table-scroll" key={index}><table className="office-doc-table" style={{ width: width > 0 ? `${width}pt` : "100%", tableLayout: width > 0 ? "fixed" : "auto", marginLeft: block.alignment === "left" ? `${block.indent}pt` : "auto", marginRight: block.alignment === "center" ? "auto" : undefined }}>{width > 0 && <colgroup>{block.columnWidths.map((column, columnIndex) => <col key={columnIndex} style={{ width: `${column}pt` }} />)}</colgroup>}<tbody>{block.rows.map((row, rowIndex) => <tr key={rowIndex} style={{ height: row.height === undefined ? undefined : `${row.height}pt` }}>{row.cells.map((cell, cellIndex) => <td key={cellIndex} colSpan={cell.columnSpan} rowSpan={cell.rowSpan} style={{ ...borders(cell.borders), backgroundColor: color(cell.shading), padding: `${cell.margins.top}pt ${cell.margins.right}pt ${cell.margins.bottom}pt ${cell.margins.left}pt`, verticalAlign: cell.verticalAlign === "center" ? "middle" : cell.verticalAlign }}><Blocks blocks={cell.blocks} /></td>)}</tr>)}</tbody></table></div>;
    }
    const lineHeight = block.lineSpacing.rule === "auto" ? block.lineSpacing.value : `${block.lineSpacing.value}pt`;
    return <div key={index} role={block.headingLevel ? "heading" : undefined} aria-level={block.headingLevel} className="office-paragraph" style={{ ...borders(block.borders), backgroundColor: color(block.shading), textAlign: block.alignment, margin: `${block.spacingBefore}pt ${block.indentRight}pt ${block.spacingAfter}pt ${block.indentLeft}pt`, textIndent: `${block.indentFirstLine}pt`, lineHeight, ...(!block.inlines.length ? runStyle(block.mark) : {}) }}>
      {block.marker && <span style={runStyle(block.marker.run)}>{block.marker.text}{block.marker.suffix === "tab" ? "\u2003" : block.marker.suffix === "space" ? " " : ""}</span>}
      {block.inlines.map((inline, inlineIndex) => inline.kind === "text" ? <span key={inlineIndex} style={runStyle(inline.run)}>{inline.text}</span> : inline.kind === "tab" ? <span key={inlineIndex} style={runStyle(inline.run)}>{"\u2003"}</span> : inline.kind === "break" ? <br key={inlineIndex} /> : null)}
      {!block.inlines.length && <br />}
    </div>;
  })}</>;
}

export default function DocxContent({ doc, title }: { doc: DocxDocument; title: string }) {
  const zh = useLocaleTag() === "zh-TW";
  return <section className="office-preview" aria-label={title}>
    <p className="office-notice">{zh ? "唯讀預覽；版面可能與原始文件不同。圖片、連結目標及不支援的內容不會顯示。" : "Read-only preview; layout may differ from the original. Images, link destinations and unsupported content are not shown."}{doc.truncated && (zh ? " 部分內容因預覽限制未顯示。" : " Some content is omitted by preview limits.")}{doc.omitted.length > 0 && (zh ? " 文件中的其他內容（包括可能的註腳或尾註）未顯示。" : " Other document content, including any footnotes or endnotes, is omitted.")}</p>
    {doc.body.length || doc.header.length || doc.footer.length ? <div className="office-doc-scroll" tabIndex={0} role="region" aria-label={zh ? "文件內容" : "Document contents"}><div className="office-document"><header><Blocks blocks={doc.header} /></header><div><Blocks blocks={doc.body} /></div><footer><Blocks blocks={doc.footer} /></footer></div></div> : <div className="office-status">{zh ? "此文件沒有可顯示的內容。" : "This document has no content to display."}</div>}
  </section>;
}
