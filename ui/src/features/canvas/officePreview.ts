// Zero-dependency client-side Office (.docx / .pptx) ZIP + XML preview parser.
// Runs in both browser and Node 22 via Web Streams `DecompressionStream("deflate-raw")`.

const ZIP_MAX_ENTRIES = 200;
const ZIP_MAX_UNCOMPRESSED_BYTES = 10 * 1024 * 1024;
const DOCX_MAX_BLOCKS = 500;
const PPTX_MAX_SLIDES = 100;

export type DocxBlock =
  | { kind: "heading"; level: 1 | 2 | 3; text: string }
  | { kind: "paragraph"; text: string }
  | { kind: "list_item"; text: string }
  | { kind: "table"; rows: string[][] };

export interface DocxPreview {
  blocks: DocxBlock[];
  truncated: boolean;
}

export interface PptxSlide {
  index: number;
  title: string;
  lines: string[];
}

export interface PptxPreview {
  slides: PptxSlide[];
  truncated: boolean;
}

const utf8 = new TextDecoder();

function readU16(view: DataView, offset: number): number {
  return view.getUint16(offset, true);
}

function readU32(view: DataView, offset: number): number {
  return view.getUint32(offset, true);
}

async function inflateRaw(compressed: Uint8Array, expectedSize: number): Promise<Uint8Array> {
  if (typeof DecompressionStream === "undefined") {
    throw new Error("DecompressionStream unavailable");
  }
  const copy = new Uint8Array(compressed.byteLength);
  copy.set(compressed);
  const stream = new Blob([copy.buffer]).stream().pipeThrough(new DecompressionStream("deflate-raw"));
  const reader = stream.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.byteLength;
      if (total > ZIP_MAX_UNCOMPRESSED_BYTES || (expectedSize > 0 && total > expectedSize + 65536)) {
        await reader.cancel();
        throw new Error("ZIP entry exceeds size limit");
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  const out = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    out.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return out;
}

async function readZipTextEntries(
  buffer: ArrayBuffer,
  wanted: (name: string) => boolean,
): Promise<Map<string, string>> {
  const view = new DataView(buffer);
  const bytes = new Uint8Array(buffer);
  const len = bytes.byteLength;
  if (len < 22) throw new Error("Not a valid ZIP archive");

  // Search for End of Central Directory (0x06054b50) near end of file.
  const searchStart = Math.max(0, len - 65557);
  let eocdOffset = -1;
  for (let i = len - 22; i >= searchStart; i--) {
    if (readU32(view, i) === 0x06054b50) {
      eocdOffset = i;
      break;
    }
  }
  if (eocdOffset < 0) throw new Error("Missing ZIP end of central directory");

  const entryCount = readU16(view, eocdOffset + 10);
  const cdSize = readU32(view, eocdOffset + 12);
  const cdOffset = readU32(view, eocdOffset + 16);
  if (entryCount > ZIP_MAX_ENTRIES || cdOffset + cdSize > len) {
    throw new Error("ZIP archive exceeds limits or is truncated");
  }

  const out = new Map<string, string>();
  let totalUncompressed = 0;
  let cursor = cdOffset;
  for (let i = 0; i < entryCount; i++) {
    if (cursor + 46 > len || readU32(view, cursor) !== 0x02014b50) {
      throw new Error("Invalid ZIP central directory header");
    }
    const method = readU16(view, cursor + 10);
    const compressedSize = readU32(view, cursor + 20);
    const uncompressedSize = readU32(view, cursor + 24);
    const nameLen = readU16(view, cursor + 28);
    const extraLen = readU16(view, cursor + 30);
    const commentLen = readU16(view, cursor + 32);
    const localHeaderOffset = readU32(view, cursor + 42);
    const nameStart = cursor + 46;
    if (nameStart + nameLen > len) throw new Error("Truncated ZIP entry name");
    const name = utf8.decode(bytes.subarray(nameStart, nameStart + nameLen));
    cursor = nameStart + nameLen + extraLen + commentLen;

    if (!wanted(name)) continue;
    totalUncompressed += uncompressedSize;
    if (totalUncompressed > ZIP_MAX_UNCOMPRESSED_BYTES) {
      throw new Error("Archive content exceeds size limit");
    }
    if (localHeaderOffset + 30 > len || readU32(view, localHeaderOffset) !== 0x04034b50) {
      throw new Error("Invalid ZIP local file header");
    }
    const localNameLen = readU16(view, localHeaderOffset + 26);
    const localExtraLen = readU16(view, localHeaderOffset + 28);
    const dataStart = localHeaderOffset + 30 + localNameLen + localExtraLen;
    const dataEnd = dataStart + compressedSize;
    if (dataEnd > len) throw new Error("Truncated ZIP entry data");

    const slice = bytes.subarray(dataStart, dataEnd);
    let raw: Uint8Array;
    if (method === 0) {
      raw = slice;
    } else if (method === 8) {
      raw = await inflateRaw(slice, uncompressedSize);
    } else {
      throw new Error(`Unsupported ZIP compression method ${method}`);
    }
    out.set(name, utf8.decode(raw));
  }
  return out;
}

function unescapeXml(raw: string): string {
  return raw
    .replace(/&#(\d+);/g, (_, d) => {
      const code = Number(d);
      return Number.isFinite(code) && code >= 0 && code <= 0x10ffff ? String.fromCodePoint(code) : "";
    })
    .replace(/&#x([0-9a-fA-F]+);/g, (_, h) => {
      const code = Number.parseInt(h, 16);
      return Number.isFinite(code) && code >= 0 && code <= 0x10ffff ? String.fromCodePoint(code) : "";
    })
    .replace(/&(lt|gt|quot|apos|amp);/g, (_, name) =>
      name === "lt" ? "<" : name === "gt" ? ">" : name === "quot" ? '"' : name === "apos" ? "'" : "&",
    );
}

function extractTagText(xml: string, tagName: string): string {
  const withTabOrBreak = xml
    .replace(new RegExp(`<${tagName.replace(/t$/, "br")}(?:\\s[^>]*)?/>`, "gi"), "\n")
    .replace(new RegExp(`<${tagName.replace(/t$/, "tab")}(?:\\s[^>]*)?/>`, "gi"), " ");
  const re = new RegExp(`<${tagName}(?:\\s[^>]*)?>([\\s\\S]*?)</${tagName}>`, "g");
  let text = "";
  let match: RegExpExecArray | null;
  while ((match = re.exec(withTabOrBreak)) !== null) {
    text += unescapeXml(match[1] ?? "");
  }
  return text.replace(/[ \t]+\n/g, "\n").trim();
}

function parseDocxParagraph(pXml: string): DocxBlock | null {
  const text = extractTagText(pXml, "w:t");
  if (!text) return null;
  const styleMatch = /<w:pStyle\b[^>]*\bw:val="([^"]+)"/i.exec(pXml);
  const style = styleMatch?.[1] ?? "";
  if (/^(?:Title|Heading1)$/i.test(style)) return { kind: "heading", level: 1, text };
  if (/^(?:Subtitle|Heading2)$/i.test(style)) return { kind: "heading", level: 2, text };
  if (/^Heading[3-6]$/i.test(style)) return { kind: "heading", level: 3, text };
  if (/<w:numPr\b/i.test(pXml)) return { kind: "list_item", text };
  return { kind: "paragraph", text };
}

function parseDocxTable(tblXml: string): DocxBlock | null {
  const rows: string[][] = [];
  const rowRe = /<w:tr\b[^>]*>([\s\S]*?)<\/w:tr>/g;
  let rMatch: RegExpExecArray | null;
  while ((rMatch = rowRe.exec(tblXml)) !== null) {
    const cells: string[] = [];
    const cellRe = /<w:tc\b[^>]*>([\s\S]*?)<\/w:tc>/g;
    let cMatch: RegExpExecArray | null;
    while ((cMatch = cellRe.exec(rMatch[1] ?? "")) !== null) {
      const pTexts: string[] = [];
      const pRe = /<w:p\b[^>]*>([\s\S]*?)<\/w:p>/g;
      let pMatch: RegExpExecArray | null;
      while ((pMatch = pRe.exec(cMatch[1] ?? "")) !== null) {
        const line = extractTagText(pMatch[1] ?? "", "w:t");
        if (line) pTexts.push(line);
      }
      cells.push(pTexts.join("\n"));
    }
    if (cells.length > 0) rows.push(cells);
  }
  return rows.length > 0 ? { kind: "table", rows } : null;
}

/** Parse a `.docx` ZIP buffer into a read-only sequence of headings, paragraphs, list items, and tables. */
export async function parseDocxPreview(bytes: ArrayBuffer): Promise<DocxPreview> {
  const entries = await readZipTextEntries(bytes, (name) => name === "word/document.xml");
  const xml = entries.get("word/document.xml");
  if (!xml) throw new Error("Missing word/document.xml");

  const bodyMatch = /<w:body\b[^>]*>([\s\S]*)<\/w:body>/i.exec(xml);
  const body = bodyMatch?.[1] ?? xml;
  const blocks: DocxBlock[] = [];
  let truncated = false;

  // Walk top-level <w:tbl> and standalone <w:p> in document order without counting paragraphs inside tables twice.
  const tokenRe = /<w:tbl\b[^>]*>[\s\S]*?<\/w:tbl>|<w:p\b[^>]*>[\s\S]*?<\/w:p>|<w:p\b[^>]*\/>/g;
  let match: RegExpExecArray | null;
  while ((match = tokenRe.exec(body)) !== null) {
    if (blocks.length >= DOCX_MAX_BLOCKS) {
      truncated = true;
      break;
    }
    const snippet = match[0];
    const block = snippet.startsWith("<w:tbl") ? parseDocxTable(snippet) : parseDocxParagraph(snippet);
    if (block) blocks.push(block);
  }
  return { blocks, truncated };
}

/** Parse a `.pptx` ZIP buffer into a read-only list of numbered slide titles and bullet lines. */
export async function parsePptxPreview(bytes: ArrayBuffer): Promise<PptxPreview> {
  const slideNameRe = /^ppt\/slides\/slide(\d+)\.xml$/;
  const entries = await readZipTextEntries(bytes, (name) => slideNameRe.test(name));
  const sorted = Array.from(entries.entries())
    .map(([name, xml]) => ({ num: Number(slideNameRe.exec(name)?.[1] ?? 0), xml }))
    .sort((a, b) => a.num - b.num);
  if (sorted.length === 0) throw new Error("No PowerPoint slides found");

  const truncated = sorted.length > PPTX_MAX_SLIDES;
  const slides: PptxSlide[] = sorted.slice(0, PPTX_MAX_SLIDES).map(({ num, xml }) => {
    const paragraphs: string[] = [];
    const pRe = /<a:p\b[^>]*>([\s\S]*?)<\/a:p>/g;
    let match: RegExpExecArray | null;
    while ((match = pRe.exec(xml)) !== null) {
      const text = extractTagText(match[1] ?? "", "a:t");
      if (text) paragraphs.push(text);
    }
    const [title = "", ...lines] = paragraphs;
    return { index: num, title, lines };
  });
  return { slides, truncated };
}
