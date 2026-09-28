import { promptDefault, effectivePrompt, promptError } from "../src/features/workspace-apps/promptFields";
import { forgetContext, initialContext, rememberContext, rememberedContext } from "../src/features/workspace-apps/contextSelection";

function equal(actual: unknown, expected: unknown, why: string) {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) throw new Error(`${why}: ${JSON.stringify(actual)} !== ${JSON.stringify(expected)}`);
}
function assert(value: unknown, why: string) { if (!value) throw new Error(why); }

const appInputs = [{ name: "content_prompt", default: "Write a concise caption grounded in facts" }, { name: "image_prompt", default: "Make a clear image grounded in facts" }];
const brand = { content_prompt: "Use a calm voice", image_prompt: "Use warm colors" };
equal(promptDefault("content_prompt", appInputs, undefined), { value: appInputs[0].default, origin: "app_default" }, "Context-free post uses installed app default");
equal(promptDefault("content_prompt", appInputs, brand), { value: brand.content_prompt, origin: "context_default" }, "Selected brand replaces app default");
equal(promptDefault("content_prompt", appInputs, { content_prompt: "" }), { value: appInputs[0].default, origin: "app_default" }, "Blank saved brand field falls back to app default");
equal(effectivePrompt("Use a playful voice", promptDefault("content_prompt", appInputs, brand)), { value: "Use a playful voice", origin: "run_override" }, "New-post override is per run");
equal(effectivePrompt("", promptDefault("content_prompt", appInputs, brand)), { value: brand.content_prompt, origin: "context_default" }, "Clearing override resets to saved default");
equal(effectivePrompt("", promptDefault("image_prompt", appInputs, undefined)), { value: appInputs[1].default, origin: "app_default" }, "Context-free image keeps app default");
equal(promptError("Use the selected source post", "Image prompt"), null, "Ordinary single-line prompt passes");
assert(promptError("Line one\nLine two", "Content prompt") !== null, "Multiline prompt is refused");
assert(promptError("Hidden\u200btext", "Content prompt") !== null, "Invisible character is refused");
assert(promptError("字".repeat(700), "Content prompt") !== null, "Oversized prompt is refused");

const storage = new Map<string, string>();
Object.defineProperty(globalThis, "window", { configurable: true, value: { sessionStorage: {
  getItem: (key: string) => storage.get(key) ?? null,
  setItem: (key: string, value: string) => storage.set(key, value),
  removeItem: (key: string) => storage.delete(key),
} } });
equal(initialContext("install-a", ["fav-limited"]), "fav-limited", "First visit selects the only active brand so its plans are visible");
equal(rememberedContext("install-a"), "fav-limited", "Selected context survives a page remount");
equal(rememberedContext("install-b"), null, "Context selection is isolated by installation");
rememberContext("install-a", "");
equal(initialContext("install-a", ["fav-limited"]), "", "Explicit No brand context survives reload even when one brand exists");
rememberContext("install-a", "stale-brand");
equal(initialContext("install-a", ["fav-limited"]), "fav-limited", "Archived selection resolves to the sole active brand");
forgetContext("install-a");
equal(rememberedContext("install-a"), null, "Access loss forgets the private context selection");
Object.defineProperty(globalThis, "window", { configurable: true, value: { sessionStorage: { getItem() { throw Error("blocked"); }, setItem() { throw Error("blocked"); }, removeItem() { throw Error("blocked"); } } } });
equal(rememberedContext("install-a"), null, "Blocked storage does not expose a stale context");
rememberContext("install-a", "fav-limited");
console.log("Social Content prompt and context selection checks passed");
