import { downloadName } from "../src/features/canvas/ArtifactView";
import { parseDocxPreview, parsePptxPreview } from "../src/features/canvas/officePreview";

export {};

function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}

function buildStoredZip(entries: Array<{ name: string; content: string }>): ArrayBuffer {
  const enc = new TextEncoder();
  const localParts: Uint8Array[] = [];
  const cdParts: Uint8Array[] = [];
  let offset = 0;

  for (const entry of entries) {
    const nameBytes = enc.encode(entry.name);
    const dataBytes = enc.encode(entry.content);

    const local = new Uint8Array(30 + nameBytes.length + dataBytes.length);
    const lv = new DataView(local.buffer);
    lv.setUint32(0, 0x04034b50, true);
    lv.setUint16(4, 20, true);
    lv.setUint16(8, 0, true);
    lv.setUint32(18, dataBytes.length, true);
    lv.setUint32(22, dataBytes.length, true);
    lv.setUint16(26, nameBytes.length, true);
    local.set(nameBytes, 30);
    local.set(dataBytes, 30 + nameBytes.length);
    localParts.push(local);

    const cd = new Uint8Array(46 + nameBytes.length);
    const cv = new DataView(cd.buffer);
    cv.setUint32(0, 0x02014b50, true);
    cv.setUint16(4, 20, true);
    cv.setUint16(6, 20, true);
    cv.setUint16(10, 0, true);
    cv.setUint32(20, dataBytes.length, true);
    cv.setUint32(24, dataBytes.length, true);
    cv.setUint16(28, nameBytes.length, true);
    cv.setUint32(42, offset, true);
    cd.set(nameBytes, 46);
    cdParts.push(cd);

    offset += local.length;
  }

  const cdSize = cdParts.reduce((sum, p) => sum + p.length, 0);
  const eocd = new Uint8Array(22);
  const ev = new DataView(eocd.buffer);
  ev.setUint32(0, 0x06054b50, true);
  ev.setUint16(8, entries.length, true);
  ev.setUint16(10, entries.length, true);
  ev.setUint32(12, cdSize, true);
  ev.setUint32(16, offset, true);

  const total = new Uint8Array(offset + cdSize + eocd.length);
  let cursor = 0;
  for (const p of localParts) { total.set(p, cursor); cursor += p.length; }
  for (const p of cdParts) { total.set(p, cursor); cursor += p.length; }
  total.set(eocd, cursor);
  return total.buffer;
}

async function deflateRawBytes(raw: Uint8Array): Promise<Uint8Array> {
  const copy = new Uint8Array(raw.byteLength);
  copy.set(raw);
  const stream = new Blob([copy.buffer]).stream().pipeThrough(new CompressionStream("deflate-raw"));
  const reader = stream.getReader();
  const chunks: Uint8Array[] = [];
  let len = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
    len += value.byteLength;
  }
  const out = new Uint8Array(len);
  let pos = 0;
  for (const c of chunks) { out.set(c, pos); pos += c.byteLength; }
  return out;
}

async function buildDeflatedZip(entries: Array<{ name: string; content: string }>): Promise<ArrayBuffer> {
  const enc = new TextEncoder();
  const localParts: Uint8Array[] = [];
  const cdParts: Uint8Array[] = [];
  let offset = 0;

  for (const entry of entries) {
    const nameBytes = enc.encode(entry.name);
    const uncompressed = enc.encode(entry.content);
    const compressed = await deflateRawBytes(uncompressed);

    const local = new Uint8Array(30 + nameBytes.length + compressed.length);
    const lv = new DataView(local.buffer);
    lv.setUint32(0, 0x04034b50, true);
    lv.setUint16(4, 20, true);
    lv.setUint16(8, 8, true);
    lv.setUint32(18, compressed.length, true);
    lv.setUint32(22, uncompressed.length, true);
    lv.setUint16(26, nameBytes.length, true);
    local.set(nameBytes, 30);
    local.set(compressed, 30 + nameBytes.length);
    localParts.push(local);

    const cd = new Uint8Array(46 + nameBytes.length);
    const cv = new DataView(cd.buffer);
    cv.setUint32(0, 0x02014b50, true);
    cv.setUint16(4, 20, true);
    cv.setUint16(6, 20, true);
    cv.setUint16(10, 8, true);
    cv.setUint32(20, compressed.length, true);
    cv.setUint32(24, uncompressed.length, true);
    cv.setUint16(28, nameBytes.length, true);
    cv.setUint32(42, offset, true);
    cd.set(nameBytes, 46);
    cdParts.push(cd);

    offset += local.length;
  }

  const cdSize = cdParts.reduce((sum, p) => sum + p.length, 0);
  const eocd = new Uint8Array(22);
  const ev = new DataView(eocd.buffer);
  ev.setUint32(0, 0x06054b50, true);
  ev.setUint16(8, entries.length, true);
  ev.setUint16(10, entries.length, true);
  ev.setUint32(12, cdSize, true);
  ev.setUint32(16, offset, true);

  const total = new Uint8Array(offset + cdSize + eocd.length);
  let cursor = 0;
  for (const p of localParts) { total.set(p, cursor); cursor += p.length; }
  for (const p of cdParts) { total.set(p, cursor); cursor += p.length; }
  total.set(eocd, cursor);
  return total.buffer;
}

async function main() {
  // 1. Unicode / CJK-safe downloadName()
  assert(
    downloadName("季度財務報告 2026 Q4", "xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet") === "季度財務報告 2026 Q4.xlsx",
    "CJK filename should be preserved",
  );
  assert(
    downloadName("Q4: Budget / Draft?.docx", "docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document") === "Q4_ Budget _ Draft_.docx",
    "Filesystem-unsafe chars should be stripped without duplicating extension",
  );
  assert(
    downloadName("...", "pdf", "application/pdf") === "artifact.pdf",
    "Dots-only title should fall back to artifact",
  );

  // 2. parseDocxPreview (stored ZIP)
  const docxBytes = buildStoredZip([
    {
      name: "word/document.xml",
      content: `<w:document><w:body>
        <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>專案提案 &amp; 時程</w:t></w:r></w:p>
        <w:p><w:r><w:t>First paragraph.</w:t></w:r></w:p>
        <w:p><w:pPr><w:numPr/></w:pPr><w:r><w:t>Bullet one</w:t></w:r></w:p>
        <w:tbl><w:tr><w:tc><w:p><w:r><w:t>Item</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Qty</w:t></w:r></w:p></w:tc></w:tr>
        <w:tr><w:tc><w:p><w:r><w:t>License</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>10</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
      </w:body></w:document>`,
    },
  ]);
  const docx = await parseDocxPreview(docxBytes);
  assert(docx.blocks.length === 4, `expected 4 docx blocks, got ${docx.blocks.length}`);
  assert(docx.blocks[0]?.kind === "heading" && docx.blocks[0].level === 1 && docx.blocks[0].text === "專案提案 & 時程", "heading decoded");
  assert(docx.blocks[1]?.kind === "paragraph" && docx.blocks[1].text === "First paragraph.", "paragraph decoded");
  assert(docx.blocks[2]?.kind === "list_item" && docx.blocks[2].text === "Bullet one", "list item decoded");
  assert(docx.blocks[3]?.kind === "table" && docx.blocks[3].rows[1]?.[0] === "License", "table decoded");

  // 3. parsePptxPreview (deflated ZIP, numeric slide sort)
  const pptxBytes = await buildDeflatedZip([
    {
      name: "ppt/slides/slide10.xml",
      content: `<p:sld><p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:t>Appendix</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>`,
    },
    {
      name: "ppt/slides/slide2.xml",
      content: `<p:sld><p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:t>Roadmap</a:t></a:r></a:p><a:p><a:r><a:t>Launch in Q2</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>`,
    },
  ]);
  const pptx = await parsePptxPreview(pptxBytes);
  assert(pptx.slides.length === 2, "expected 2 slides");
  assert(pptx.slides[0]?.index === 2 && pptx.slides[0].title === "Roadmap" && pptx.slides[0].lines[0] === "Launch in Q2", "slide 2 first");
  assert(pptx.slides[1]?.index === 10 && pptx.slides[1].title === "Appendix", "slide 10 second");

  // 4. Reject invalid ZIP
  let rejected = false;
  try {
    await parseDocxPreview(new TextEncoder().encode("not a zip archive").buffer);
  } catch {
    rejected = true;
  }
  assert(rejected, "invalid ZIP must reject");

  console.log("officePreview tests passed");
}

void main();
