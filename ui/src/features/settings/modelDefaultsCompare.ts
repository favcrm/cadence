/** Canonical JSON for dirty-checking model-default documents.

The daemon puts config through `serde_json::Value`. That map is a
`BTreeMap`, so object keys arrive sorted (`providers` before `schema`,
role ids alphabetically). The editor builds `{schema, providers}` and
inserts roles in team-role order. `JSON.stringify` would treat those
as different documents and leave Save enabled on an untouched page.
*/
export function canonicalJson(value: unknown): string {
  return JSON.stringify(sortKeys(value));
}

function sortKeys(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(sortKeys);
  if (value !== null && typeof value === "object") {
    const source = value as Record<string, unknown>;
    const sorted: Record<string, unknown> = {};
    for (const key of Object.keys(source).sort()) {
      sorted[key] = sortKeys(source[key]);
    }
    return sorted;
  }
  return value;
}
