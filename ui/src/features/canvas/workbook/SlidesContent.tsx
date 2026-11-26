import { useId, useState, type CSSProperties } from "react";
import { useLocaleTag } from "../../../lib/locale";
import type { SlideBounds, SlideColor, SlideDeck, SlideFill, SlideOutline, SlideTextBody } from "./vendor/pptx-slide-model";
import "./office-preview.css";

function paint(color: SlideColor): string {
  if (!/^[\da-f]{6}$/i.test(color.hex)) return "transparent";
  const alpha = Math.max(0, Math.min(1, color.alpha));
  return `#${color.hex}${Math.round(alpha * 255).toString(16).padStart(2, "0")}`;
}

function fill(value: SlideFill | undefined): string | undefined {
  if (!value || value.kind === "image") return undefined;
  if (value.kind === "solid") return paint(value.color);
  if (!value.stops.length) return undefined;
  return `linear-gradient(${value.angle + 90}deg, ${value.stops.map((stop) => `${paint(stop.color)} ${Math.max(0, Math.min(1, stop.position)) * 100}%`).join(", ")})`;
}

function units(value: number, width: number): string { return `${value / width * 100}cqw`; }
function outline(value: SlideOutline | undefined, width: number): string | undefined {
  return value ? `${units(value.width, width)} ${value.dash === "dash" ? "dashed" : value.dash === "dot" ? "dotted" : "solid"} ${paint(value.color)}` : undefined;
}
function bounds(box: SlideBounds, width: number, height: number): CSSProperties {
  return { position: "absolute", left: `${box.x / width * 100}%`, top: `${box.y / height * 100}%`, width: `${box.width / width * 100}%`, height: `${box.height / height * 100}%`, transform: `rotate(${box.rotation}deg) scale(${box.flipH ? -1 : 1}, ${box.flipV ? -1 : 1})` };
}

function TextBody({ body, width }: { body: SlideTextBody; width: number }) {
  return <div className="office-slide-text" style={{ padding: `${units(body.insets.top, width)} ${units(body.insets.right, width)} ${units(body.insets.bottom, width)} ${units(body.insets.left, width)}`, justifyContent: body.anchor === "middle" ? "center" : body.anchor === "bottom" ? "flex-end" : "flex-start", whiteSpace: body.wrap ? "pre-wrap" : "pre", writingMode: body.direction === "rotate90" ? "vertical-rl" : body.direction === "rotate270" ? "vertical-lr" : undefined }}>
    {body.paragraphs.map((paragraph, index) => <div key={index} style={{ textAlign: paragraph.align, marginLeft: units(paragraph.marginLeft, width), textIndent: units(paragraph.indent, width), marginTop: index ? units(paragraph.spaceBefore * body.fontScale, width) : undefined, marginBottom: units(paragraph.spaceAfter * body.fontScale, width), fontSize: units(paragraph.fontSize * body.fontScale, width), lineHeight: paragraph.lineSpacing.kind === "multiple" ? paragraph.lineSpacing.value : units(paragraph.lineSpacing.points, width) }}>
      {paragraph.bullet && <span style={{ color: paint(paragraph.bullet.color), fontSize: units(paragraph.bullet.size * body.fontScale, width) }}>{paragraph.bullet.text} </span>}
      {paragraph.runs.map((run, runIndex) => <span key={runIndex} style={{ fontFamily: /^[\w ,'-]+$/.test(run.font) ? `"${run.font}"` : undefined, fontSize: units(run.size * body.fontScale, width), fontWeight: run.bold ? "bold" : undefined, fontStyle: run.italic ? "italic" : undefined, textDecorationLine: [run.underline && "underline", run.strike && "line-through"].filter(Boolean).join(" ") || undefined, color: paint(run.color), verticalAlign: run.baseline > 0 ? "super" : run.baseline < 0 ? "sub" : undefined, letterSpacing: units(run.letterSpacing, width) }}>{run.text}</span>)}
      {!paragraph.runs.length && "\u00a0"}
    </div>)}
  </div>;
}

export default function SlidesContent({ deck, title }: { deck: SlideDeck; title: string }) {
  const zh = useLocaleTag() === "zh-TW";
  const id = useId();
  const [selected, setSelected] = useState(0);
  const slide = deck.slides[Math.min(selected, deck.slides.length - 1)];
  const width = deck.width > 0 ? deck.width : 1;
  const height = deck.height > 0 ? deck.height : 1;
  return <section className="office-preview" aria-label={title}>
    {slide && <div className="office-toolbar"><label htmlFor={id}>{zh ? "投影片" : "Slide"}</label><select id={id} value={Math.min(selected, deck.slides.length - 1)} onChange={(event) => setSelected(Number(event.target.value))}>{deck.slides.map((item, index) => <option key={index} value={index}>{zh ? `投影片 ${item.number}` : `Slide ${item.number}`}</option>)}</select></div>}
    <p className="office-notice">{zh ? "唯讀預覽；版面可能與原始簡報不同。圖片及不支援的內容不會顯示。" : "Read-only preview; layout may differ from the original. Images and unsupported content are not shown."}{deck.truncated && (zh ? " 部分投影片或內容因預覽限制未顯示。" : " Some slides or content are omitted by preview limits.")}{deck.unsupported.length > 0 && (zh ? " 圖表、圖解或內嵌物件等內容可能未顯示。" : " Content such as charts, diagrams or embedded objects may be omitted.")}</p>
    {!slide ? <div className="office-status">{zh ? "此簡報沒有可顯示的投影片。" : "This presentation has no visible slides."}</div> : <div className="office-slide-scroll"><div className="office-slide" role="group" aria-label={zh ? `投影片 ${slide.number}` : `Slide ${slide.number}`} style={{ aspectRatio: `${width} / ${height}`, background: fill(slide.background) ?? "#fff" }}>
      {slide.shapes.map((shape, index) => {
        if (shape.kind === "picture" || shape.kind === "unsupported") return null;
        if (shape.kind === "shape") return <div key={index} style={{ ...bounds(shape.bounds, width, height), boxSizing: "border-box", background: fill(shape.fill), border: outline(shape.outline, width), borderRadius: shape.geometry === "ellipse" ? "50%" : shape.geometry === "roundRect" ? units(shape.cornerRadius, width) : undefined }}>{shape.text && <TextBody body={shape.text} width={width} />}</div>;
        return <div key={index} style={{ ...bounds(shape.bounds, width, height), overflow: "hidden" }}><table className="office-slide-table"><colgroup>{shape.columnWidths.map((column, columnIndex) => <col key={columnIndex} style={{ width: `${column / (shape.columnWidths.reduce((sum, item) => sum + item, 0) || 1) * 100}%` }} />)}</colgroup><tbody>{shape.rows.map((row, rowIndex) => <tr key={rowIndex} style={{ height: units(row.height, width) }}>{row.cells.map((cell) => <td key={cell.column} colSpan={cell.columnSpan} rowSpan={cell.rowSpan} style={{ background: fill(cell.fill), borderTop: outline(cell.borders.top, width), borderRight: outline(cell.borders.right, width), borderBottom: outline(cell.borders.bottom, width), borderLeft: outline(cell.borders.left, width) }}><TextBody body={cell.text} width={width} /></td>)}</tr>)}</tbody></table></div>;
      })}
    </div></div>}
  </section>;
}
