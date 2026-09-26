/* fmt.ts — time helpers in the app's display zone (settings.timezone).
 * Ported from the standalone prototype's dom.js; the mock clock is HKT.
 */
import { api } from "./mock/api";

const TZ = "Asia/Hong_Kong";
const part = (iso: string, o: Intl.DateTimeFormatOptions) =>
  new Intl.DateTimeFormat("en-GB", { timeZone: TZ, ...o }).format(new Date(iso));
const DOWS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const dowName = (iso: string) => part(iso, { weekday: "short" });

export const fmt = {
  /** "Thu 09:30" / "Fri 25/9" in the app's pretend HKT clock. */
  time(iso: string) {
    return `${dowName(iso)} ${part(iso, { hour: "2-digit", minute: "2-digit", hour12: false })}`;
  },
  day(iso: string) {
    return `${dowName(iso)} ${part(iso, { day: "numeric", month: "numeric" })}`;
  },
  /** "2026/09/25" — sortable key. */
  dkey(iso: string) {
    return part(iso, { year: "numeric", month: "2-digit", day: "2-digit" });
  },
  /** For <input type="datetime-local"> — HK wall clock. */
  inputLocal(iso: string) {
    const d = part(iso, { year: "numeric", month: "2-digit", day: "2-digit" }).split("/").reverse().join("-");
    return d + "T" + part(iso, { hour: "2-digit", minute: "2-digit", hour12: false });
  },
  /** Monday-start day keys (HKT) for the calendar. */
  weekOf(iso: string) {
    const [dd, mm, yyyy] = part(iso, { year: "numeric", month: "2-digit", day: "2-digit" })
      .split("/")
      .map(Number); // DD/MM/YYYY
    const wd = DOWS.indexOf(dowName(iso));
    const monday = new Date(Date.UTC(yyyy, mm - 1, dd - ((wd + 6) % 7)));
    return Array.from({ length: 7 }, (_, i) => new Date(monday.getTime() + i * 86400e3));
  },
  hkDayKey(dateObj: Date) {
    return new Intl.DateTimeFormat("en-GB", { timeZone: TZ, year: "numeric", month: "2-digit", day: "2-digit" }).format(dateObj);
  },
  ago(iso: string) {
    const m = Math.round((api.NOW().getTime() - new Date(iso).getTime()) / 60000);
    if (m < 1) return "just now";
    if (m < 60) return m + "m ago";
    const h = Math.round(m / 60);
    if (h < 24) return h + "h ago";
    return Math.round(h / 24) + "d ago";
  },
  in_(iso: string) {
    const m = Math.round((new Date(iso).getTime() - api.NOW().getTime()) / 60000);
    if (m < 0) return fmt.ago(iso);
    if (m < 60) return "in " + m + "m";
    const h = Math.round(m / 60);
    if (h < 24) return "in " + h + "h";
    return "in " + Math.round(h / 24) + "d";
  },
};

/* Word-ish diff for captions: CJK chars are their own tokens, latin words
 * stay whole, whitespace is preserved. Small LCS — captions are short. */
export function tokenize(s: string): string[] {
  const out: string[] = [];
  for (const ch of s) {
    if (/\s/.test(ch)) out.push(ch);
    else if (/[⺀-鿿豈-﫿]/.test(ch)) out.push(ch);
    else if (out.length && /[\w$@#.-]/.test(out[out.length - 1]) && /[\w$@#.-]/.test(ch)) out[out.length - 1] += ch;
    else out.push(ch);
  }
  return out;
}
export type DiffTok = ["=" | "-" | "+", string];
export function diff(a: string, b: string): DiffTok[] {
  const A = tokenize(a), B = tokenize(b);
  const n = A.length, m = B.length;
  const dp = Array.from({ length: n + 1 }, () => new Uint16Array(m + 1));
  for (let i = n - 1; i >= 0; i--)
    for (let j = m - 1; j >= 0; j--)
      dp[i][j] = A[i] === B[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
  const res: DiffTok[] = [];
  let i = 0, j = 0;
  while (i < n && j < m) {
    if (A[i] === B[j]) {
      res.push(["=", A[i]]);
      i++;
      j++;
    } else if (dp[i + 1][j] >= dp[i][j + 1]) {
      res.push(["-", A[i]]);
      i++;
    } else {
      res.push(["+", B[j]]);
      j++;
    }
  }
  while (i < n) res.push(["-", A[i++]]);
  while (j < m) res.push(["+", B[j++]]);
  return res;
}
