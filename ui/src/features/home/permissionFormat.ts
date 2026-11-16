import type { MasterPermissionRule } from "../../lib/api";

/** Invisible, bidi and control characters that could reorder or hide text. */
export const INVISIBLE_PERMISSION_CHARS = /[\p{Cf}\p{Default_Ignorable_Code_Point}\p{Cc}]/gu;

/** Format/bidi/DEL/C1 chars that `JSON.stringify` leaves unescaped as raw codepoints. */
const RAW_UNESCAPED_CONTROL_CHARS = /[\p{Cf}\p{Default_Ignorable_Code_Point}\x7f-\x9f]/gu;

/** Strip control and bidi formatting characters for single-line command/path displays. */
export function cleanPermissionInline(value: string): string {
  return value.replace(/\r\n|[\r\n\t]/g, " ").replace(INVISIBLE_PERMISSION_CHARS, "");
}

/** Preserve newlines and tabs in multi-line reasons while stripping hidden control/bidi chars. */
export function cleanPermissionReason(reason: string): string {
  return reason.replace(INVISIBLE_PERMISSION_CHARS, (c) => (c === "\n" || c === "\t" ? c : ""));
}

/** Quotes preserve argument boundaries. Display only — never executed as shell syntax. */
export function formatPermissionArgument(value: string): string {
  const safe = value.replace(RAW_UNESCAPED_CONTROL_CHARS, "");
  return /^[\w./:@=+*-]+$/.test(safe) ? safe : JSON.stringify(safe);
}

/** Formats a saved `permissions.yaml` rule command uniformly for display. */
export function formatRuleCommand(
  rule: Pick<MasterPermissionRule, "argv" | "scope" | "tail">,
): string {
  return [...rule.argv, ...(rule.scope === "prefix" ? rule.tail : [])]
    .map(formatPermissionArgument)
    .join(" ");
}
