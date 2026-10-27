/**
 * Whether an install has an openable app route. Only an active row does: the
 * tenant's app route proves an active install, so a disabled or removed card
 * that offers Open sends the reader to "Unknown installation".
 */
export type InstallLifecycleState = "active" | "disabled" | "removed";

export function canOpenInstall(
  state: InstallLifecycleState | undefined,
  removed?: boolean | number | null,
): boolean {
  // `removed` arrives as a flag or as the mark's epoch; zero and null are live.
  const marked = removed !== undefined && removed !== null && removed !== false && removed !== 0;
  return !marked && (state ?? "active") === "active";
}
