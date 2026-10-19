/** A product-owned notice/error the display translates in the active
 *  locale: `key` is the English catalog key (which may carry `{name}`
 *  slots), `slots` holds the values interpolated after translation, and
 *  `detail` is raw daemon text glued on verbatim. A plain string stays
 *  raw (server/friendly error text) so existing matching keeps working.
 *  Never a pre-translated string — that would freeze the locale at
 *  completion time. */
export type UiNotice = { key: string; slots?: Record<string, string>; detail?: string } | string;

/** Render-time expansion: translate the key, interpolate slots, then
 *  append any raw detail exactly as stored. English expands to the key
 *  byte-for-byte. */
export function displayUiNotice(notice: UiNotice, t: (english: string) => string): string {
  // A raw string still goes through the catalog lookup — known keys
  // translate; daemon/friendly text misses the catalog and renders
  // verbatim, exactly like a direct t() call.
  if (typeof notice === "string") return t(notice);
  let text = t(notice.key);
  for (const [name, value] of Object.entries(notice.slots ?? {})) {
    // Replacement callback inserts `value` literally — a replacement
    // string would re-interpret $& $' $` $$ sequences inside raw
    // daemon/friendly text as special patterns.
    text = text.replace(`{${name}}`, () => value);
  }
  return notice.detail !== undefined ? `${text}${notice.detail}` : text;
}

