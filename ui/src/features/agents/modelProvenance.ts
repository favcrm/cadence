export interface ModelSelectionView {
  source?: string | null;
  lookup_role?: string | null;
  revision?: number | null;
}

type Translate = (english: string) => string;
const identity: Translate = (english) => english;

function provenanceLabel(source: string | null | undefined, t: Translate): string | null {
  switch (source) {
    case "explicit":
      return t("explicit launch model");
    case "explicit_provider_default":
      return t("explicit provider-native");
    case "role_default":
      return t("role default");
    case "provider_baseline":
      return t("provider baseline");
    case "provider_default":
      return t("provider-native default");
    case "legacy_configured":
      return t("saved before model defaults");
    case "legacy_provider_default":
      return t("saved before model defaults");
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
  t: Translate = identity,
): string | null {
  const provenance = provenanceLabel(selection?.source, t);
  const lookup = lookupRole ?? selection?.lookup_role;
  const revision = selection?.revision;
  const text = [
    provenance,
    lookup ? t("lookup {role}").replace("{role}", () => lookup) : null,
    typeof revision === "number"
      ? t("revision {number}").replace("{number}", String(revision))
      : null,
  ]
    .filter(Boolean)
    .join(" · ");
  return text || null;
}
