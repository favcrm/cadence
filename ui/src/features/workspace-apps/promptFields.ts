export type PromptName = "content_prompt" | "image_prompt";
export type PromptOrigin = "app_default" | "context_default" | "run_override";

export function promptDefault(
  name: PromptName,
  workflowInputs: { name: string; default?: string | null }[] | undefined,
  contextDefaults: Record<string, string> | undefined,
): { value: string; origin: PromptOrigin } {
  const saved = contextDefaults?.[name]?.trim();
  if (saved) return { value: saved, origin: "context_default" };
  return {
    value: workflowInputs?.find(input => input.name === name)?.default?.trim() ?? "",
    origin: "app_default",
  };
}

export function effectivePrompt(value: string, fallback: { value: string; origin: PromptOrigin }) {
  const normalized = value.trim();
  return normalized && normalized !== fallback.value
    ? { value: normalized, origin: "run_override" as const }
    : fallback;
}

export function promptError(value: string, label: string): string | null {
  if (!value) return null;
  if (value !== value.trim() || /[\x00-\x1f\x7f-\x9f\u200b-\u200f\u202a-\u202e\u2060-\u206f\ufeff]/u.test(value)
    || /\s/u.test(value.replaceAll(" ", "")))
    return `${label} must be one trimmed line with ordinary spaces only.`;
  if ([...value].length > 512 || new TextEncoder().encode(value).length > 2048)
    return `${label} must fit 512 characters and 2,048 UTF-8 bytes.`;
  return null;
}
