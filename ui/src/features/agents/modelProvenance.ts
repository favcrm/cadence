export interface ModelSelectionView {
  source?: string | null;
  lookup_role?: string | null;
  revision?: number | null;
}

function provenanceLabel(source: string | null | undefined): string | null {
  switch (source) {
    case "explicit":
      return "explicit launch model";
    case "explicit_provider_default":
      return "explicit provider-native";
    case "role_default":
      return "role default";
    case "provider_baseline":
      return "provider baseline";
    case "provider_default":
      return "provider-native default";
    case "legacy_configured":
      return "saved before model defaults";
    case "legacy_provider_default":
      return "saved before model defaults";
    default:
      return null;
  }
}

/** Provenance line for an agent. A numeric revision, including 0, is the
 * settings snapshot that was applied. Explicit and legacy selections
 * store null and must not claim a config revision.
 */
export function provenanceDetail(
  selection: ModelSelectionView | null | undefined,
  lookupRole?: string | null,
): string | null {
  const provenance = provenanceLabel(selection?.source);
  const lookup = lookupRole ?? selection?.lookup_role;
  const revision = selection?.revision;
  const text = [
    provenance,
    lookup ? `lookup ${lookup}` : null,
    typeof revision === "number" ? `revision ${revision}` : null,
  ]
    .filter(Boolean)
    .join(" · ");
  return text || null;
}
