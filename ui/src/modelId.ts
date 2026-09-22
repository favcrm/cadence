/** Same rules as the daemon's `validate_model_id`: trimmed, at most 200
 * UTF-8 bytes, and no Unicode control character (category Cc, including
 * DEL and C1). JavaScript string length is UTF-16 and would accept a
 * shorter-looking string that the server rejects.
 */
export function modelIdProblem(value: string): string | null {
  const model = value.trim();
  if (!model) return "Enter a model id.";
  if (new TextEncoder().encode(model).length > 200) {
    return "Model ids are at most 200 bytes.";
  }
  for (const ch of model) {
    const code = ch.codePointAt(0) ?? 0;
    if (code <= 0x1f || (code >= 0x7f && code <= 0x9f)) {
      return "Model ids cannot contain control characters.";
    }
  }
  return null;
}
