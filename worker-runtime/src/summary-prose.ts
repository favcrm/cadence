/** Find whole claims in prose, not inside Markdown code, block quotes or inline quotations. */
export function* proseClauses(text: string, semicolons: boolean): Generator<readonly [start: number, end: number]> {
  let start = 0;
  let quoted = false;
  let codeClosers: Map<number, number> | undefined;
  let unclosedQuotes: Set<string> | undefined;
  for (let at = 0; at < text.length;) {
    const mark = text[at]!;
    if (at === start && (at === 0 || text[at - 1] === "\n") && isCodeIndent(text, at)) {
      start = at = indentedCodeEnd(text, at);
      continue;
    }
    if (mark === ">" && blockStart(text, at)) {
      if (!quoted && start < at) yield [start, at];
      start = at = blockQuoteEnd(text, at);
      quoted = false;
      continue;
    }
    if (mark === "`" || mark === "~") {
      const after = runEnd(text, at, mark);
      const width = after - at;
      if (width >= 3 && blockStart(text, at)) {
        if (!quoted && start < at) yield [start, at];
        start = at = fenceEnd(text, after, mark, width);
        quoted = false;
        continue;
      }
      if (mark === "`") {
        const close = (codeClosers ??= indexCodeClosers(text, at)).get(at);
        if (close !== undefined) {
          // Keep a backtick-quoted tool identifier, not arbitrary code or prose.
          if (width !== 1 || !/^[A-Za-z_][\w.-]*$/.test(text.slice(after, close))) quoted = true;
          at = close + width;
          continue;
        }
      }
      at = after;
      continue;
    }
    const close = QUOTE_CLOSER[mark];
    if (close !== undefined) {
      let opener = at;
      while (text[opener - 1] === "\\") opener--;
      const escaped = (at - opener) % 2 === 1;
      const kind = escaped ? `\\${close}` : close;
      if (!unclosedQuotes?.has(kind) && !literalQuote(mark, text[opener - 1])) {
        const end = quoteEnd(text, at + 1, close, escaped);
        if (end !== undefined) {
          quoted = true;
          at = end;
          continue;
        }
        // Each delimiter kind can scan to the end unsuccessfully at most once.
        (unclosedQuotes ??= new Set()).add(kind);
      }
    }
    if (mark === "\\") {
      const after = runEnd(text, at, mark);
      // An escaped quote still frames visible prose. Skip only escaped
      // sentence punctuation and let the next iteration inspect any quote.
      at = after + ((after - at) % 2 === 1 && /[;.!?]/.test(text[after] ?? "") ? 1 : 0);
      continue;
    } else if (mark === ";" && semicolons) {
      if (!quoted && start < at) yield [start, at];
      start = at + 1;
      quoted = false;
    } else if (/[.!?]/.test(mark) && (at + 1 === text.length || /\s/.test(text[at + 1]!))) {
      if (!quoted) yield [start, at + 1];
      start = at + 1;
      quoted = false;
    } else if (mark === "\n") {
      const blank = blankLineEnd(text, at + 1);
      if (blank !== undefined) {
        if (!quoted && start < at) yield [start, at];
        start = at = blank;
        quoted = false;
        continue;
      }
    }
    at++;
  }
  if (!quoted && start < text.length) yield [start, text.length];
}

function blockStart(text: string, at: number): boolean {
  for (let spaces = 0; spaces < 3 && text[at - 1] === " "; spaces++) at--;
  return at === 0 || text[at - 1] === "\n";
}

function blankLineEnd(text: string, at: number): number | undefined {
  while (text[at] === " " || text[at] === "\t" || text[at] === "\r") at++;
  return text[at] === "\n" ? at + 1 : undefined;
}

function blockQuoteEnd(text: string, start: number): number {
  for (let line = text.indexOf("\n", start); line !== -1; line = text.indexOf("\n", line + 1)) {
    const end = blankLineEnd(text, line + 1);
    if (end !== undefined) return end;
  }
  return text.length;
}

function isCodeIndent(text: string, at: number): boolean {
  return text[at] === "\t" || text.startsWith("    ", at);
}

function indentedCodeEnd(text: string, start: number): number {
  let at = start;
  do {
    const newline = text.indexOf("\n", at);
    if (newline === -1) return text.length;
    at = newline + 1;
    for (;;) {
      const blank = blankLineEnd(text, at);
      if (blank === undefined) break;
      at = blank;
    }
  } while (isCodeIndent(text, at));
  return at;
}

function runEnd(text: string, at: number, mark: string): number {
  while (text[at] === mark) at++;
  return at;
}

function fenceEnd(text: string, start: number, mark: string, width: number): number {
  for (let newline = text.indexOf("\n", start); newline !== -1;) {
    let at = newline + 1;
    while (at <= newline + 3 && text[at] === " ") at++;
    const end = runEnd(text, at, mark);
    const next = text.indexOf("\n", end);
    if (end - at >= width && text.slice(end, next < 0 ? text.length : next).trim() === "")
      return next < 0 ? text.length : next + 1;
    newline = next;
  }
  return text.length;
}

/** Pair equal-width runs within a paragraph once; unmatched backticks remain literal. */
function indexCodeClosers(text: string, start: number): Map<number, number> {
  const lastByWidth = new Map<number, number>();
  const closers = new Map<number, number>();
  const markers = /^ {0,3}(?:`{3,}|~{3,})|`+|\n(?:[ \t\r]*\n)+/gm;
  markers.lastIndex = start;
  for (const match of text.matchAll(markers)) {
    const marker = match[0];
    if (!marker.startsWith("`") || (marker.length >= 3 && blockStart(text, match.index))) {
      lastByWidth.clear();
      continue;
    }
    const prior = lastByWidth.get(marker.length);
    if (prior !== undefined) closers.set(prior, match.index);
    lastByWidth.set(marker.length, match.index);
  }
  return closers;
}

function literalQuote(mark: string, before: string | undefined): boolean {
  if (mark === "'" || mark === "‘") return word(before);
  return mark === '"' && before !== undefined && /\d/.test(before);
}

const QUOTE_CLOSER: Readonly<Record<string, string>> = {
  '"': '"', "'": "'", "“": "”", "‘": "’", "«": "»", "‹": "›", "「": "」", "『": "』",
};

function word(char: string | undefined): boolean {
  return char !== undefined && /[\p{L}\p{N}_]/u.test(char);
}

function quoteEnd(text: string, start: number, close: string, escapedOpener: boolean): number | undefined {
  const apostrophe = close === "'" || close === "’";
  for (let at = start; at < text.length; at++) {
    const escaped = text[at] === "\\";
    const candidate = escaped ? at + 1 : at;
    if ((!escaped || escapedOpener) && text[candidate] === close &&
      !(apostrophe && word(text[at - 1]) && word(text[candidate + 1]))) return candidate + 1;
    if (escaped) at++;
  }
}
