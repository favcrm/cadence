/**
 * CAD-2378: the company time zone and a person's display zone.
 *
 * The company zone decides when every schedule runs. A person's display zone
 * changes only how times are shown to them, and always with its label. Both
 * come from `/api/time-zone`; a board that predates it answers 404, and the
 * UI then reads UTC rather than guessing the browser's zone.
 */
import { useEffect, useSyncExternalStore } from "react";
import { ApiError, errorMessage } from "./apiError";
import { sessionHeaders } from "./sessionKey";
import { privateFetch, privateReadsAllowed, usingPrivateOperation } from "./privateSession";

export interface TimeZones {
  /** The company zone, or null while unread or on a board without the route. */
  company: string | null;
  /** Where the company zone came from: the owner, a social connection, or the UTC default. */
  source: "workspace" | "social_connection" | "default" | null;
  /** This person's own choice, or null to follow the company zone. */
  display: string | null;
  /** The zone times are shown in. */
  effective: string;
  canSetCompany: boolean;
  loaded: boolean;
}

const FALLBACK = "UTC";

/** True for a zone name this runtime's IANA database knows. */
export function validZone(name: string): boolean {
  if (!name || name !== name.trim()) return false;
  try {
    new Intl.DateTimeFormat("en-US", { timeZone: name });
    return true;
  } catch {
    return false;
  }
}

const SECOND = 1000;

function instant(value: number | string): Date | null {
  const ms = typeof value === "number" ? value * SECOND : Date.parse(value);
  return Number.isFinite(ms) ? new Date(ms) : null;
}

export interface ZonedClock {
  /** `HH:mm` on the zone's clock. */
  time: string;
  /** `YYYY-MM-DD` on the zone's calendar. */
  day: string;
  /** Short zone name at that instant, such as `GMT` or `HKT`. */
  label: string;
}

/** The wall clock, date and label an instant has in `zone`. */
export function clockInZone(value: number | string, zone: string): ZonedClock | null {
  const date = instant(value);
  if (!date || !validZone(zone)) return null;
  const parts = new Intl.DateTimeFormat("en-GB", {
    timeZone: zone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
    timeZoneName: "short",
  }).formatToParts(date);
  const get = (type: string) => parts.find((p) => p.type === type)?.value ?? "";
  return {
    time: `${get("hour")}:${get("minute")}`,
    day: `${get("year")}-${get("month")}-${get("day")}`,
    label: get("timeZoneName") || zone,
  };
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** `17 Nov 01:00 GMT`: an instant in `zone`, labelled with the zone. */
export function fmtInZone(value: number | string, zone: string | null | undefined): string | null {
  const clock = clockInZone(value, zone && validZone(zone) ? zone : FALLBACK);
  if (!clock) return null;
  const [, month, day] = clock.day.split("-");
  return `${Number(day)} ${MONTHS[Number(month) - 1]} ${clock.time} ${clock.label}`;
}

/**
 * A recurring `HH:mm` summary as the viewer sees it: the next run's clock time
 * in the viewer's zone, but only when that run falls on the same calendar day
 * in both zones. A day shift (a Monday 09:00 run that is Sunday for the
 * viewer) cannot be described by the stored weekday, so the caller keeps the
 * routine's own zone text instead.
 */
export function sameDayClock(
  next: number | string,
  routineZone: string,
  viewerZone: string,
): ZonedClock | null {
  const own = clockInZone(next, routineZone);
  const mine = clockInZone(next, viewerZone);
  return own && mine && own.day === mine.day ? mine : null;
}

const listeners = new Set<() => void>();
let state: TimeZones = {
  company: null,
  source: null,
  display: null,
  effective: FALLBACK,
  canSetCompany: false,
  loaded: false,
};
let inflight: Promise<void> | null = null;

function publish(next: TimeZones): void {
  state = next;
  for (const listener of [...listeners]) listener();
}

function decode(value: unknown): TimeZones {
  const body = value !== null && typeof value === "object" ? (value as Record<string, unknown>) : {};
  const zone = (key: string) => {
    const raw = body[key];
    return typeof raw === "string" && validZone(raw) ? raw : null;
  };
  const company = zone("company_timezone");
  const display = zone("display_timezone");
  const source = body.company_timezone_source;
  return {
    company,
    source: source === "workspace" || source === "social_connection" || source === "default" ? source : null,
    display,
    effective: display ?? company ?? FALLBACK,
    canSetCompany: body.can_set_company === true,
    loaded: true,
  };
}

async function request(init?: RequestInit): Promise<TimeZones> {
  return usingPrivateOperation(undefined, async (op) => {
    const resp = await privateFetch(
      "/api/time-zone",
      {
        ...init,
        headers: { "X-Cadence-Board": "1", ...(init?.body ? { "Content-Type": "application/json" } : {}), ...sessionHeaders() },
      },
      op,
    );
    const body: unknown = await resp.json().catch(() => null);
    if (!resp.ok) {
      const record = body !== null && typeof body === "object" && !Array.isArray(body) ? (body as Record<string, unknown>) : undefined;
      throw new ApiError(errorMessage(body, `${resp.status} ${resp.statusText}`), resp.status, record);
    }
    op.assertCurrent();
    return decode(body);
  });
}

/** Read the zones once; a board without the route settles as loaded with the UTC fallback. */
export function loadTimeZones(): Promise<void> {
  if (!privateReadsAllowed()) return Promise.resolve();
  inflight ??= request()
    .then(publish)
    .catch(() => publish({ ...state, loaded: true }))
    .finally(() => {
      inflight = null;
    });
  return inflight;
}

async function save(scope: "company" | "user", timezone: string | null): Promise<void> {
  publish(await request({ method: "POST", body: JSON.stringify({ scope, timezone }) }));
}

export const setCompanyTimeZone = (zone: string) => save("company", zone);
export const setDisplayTimeZone = (zone: string | null) => save("user", zone);

const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => listeners.delete(listener);
};

/** The zones, read on first use. */
export function useTimeZones(): TimeZones {
  useEffect(() => {
    void loadTimeZones();
  }, []);
  return useSyncExternalStore(subscribe, () => state);
}

/** Test seam: reset the module store. */
export function resetTimeZonesForTest(next?: Partial<TimeZones>): void {
  inflight = null;
  state = {
    company: null,
    source: null,
    display: null,
    effective: FALLBACK,
    canSetCompany: false,
    loaded: false,
    ...next,
  };
}
