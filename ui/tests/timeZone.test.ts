import { clockInZone, fmtInZone, sameDayClock, validZone } from "../src/lib/timeZone";

function eq(actual: unknown, expected: unknown, what: string): void {
  if (actual !== expected) throw new Error(`${what}: expected ${String(expected)}, got ${String(actual)}`);
}

// A routine at 09:00 Asia/Hong_Kong fires at 01:00Z in November.
const nine = Date.UTC(2026, 10, 17, 1, 0, 0) / 1000;

const hk = clockInZone(nine, "Asia/Hong_Kong");
eq(hk?.time, "09:00", "company clock");

const london = sameDayClock(nine, "Asia/Hong_Kong", "Europe/London");
eq(london?.time, "01:00", "member clock for 09:00 company time");
eq(london?.label, "GMT", "member zone label");
eq(fmtInZone(nine, "Europe/London"), "17 Nov 01:00 GMT", "labelled instant");

// A viewer for whom the run lands on the previous day gets no rewritten weekday.
eq(sameDayClock(nine, "Asia/Hong_Kong", "America/Los_Angeles"), null, "day shift keeps the routine's own text");

// An unknown zone falls back to UTC for display rather than the browser's zone.
eq(fmtInZone(nine, "Not/AZone"), "17 Nov 01:00 UTC", "unknown zone shows UTC");
eq(fmtInZone("2026-11-17T01:00:00Z", "Asia/Hong_Kong")?.startsWith("17 Nov 09:00"), true, "ISO input");

eq(validZone("Asia/Hong_Kong"), true, "valid IANA name");
eq(validZone("Mars/Base"), false, "invalid IANA name");
eq(validZone(""), false, "empty name");
