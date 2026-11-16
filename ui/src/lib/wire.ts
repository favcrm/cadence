/**
 * One canonical JSON-object guard for wire payloads this SPA validates.
 *
 * It proves the *container* only; every field is still checked by the caller
 * in its own type. Shared so a payload reader never grows a home-grown copy.
 */
export function objectField(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}
