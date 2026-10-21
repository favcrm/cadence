/**
 * Structural checks for an app bundle: the manifest, its views, workflows,
 * rubrics and the assets the listing names. This is deliberately not a YAML
 * or JSON-schema engine — it reads the shapes the platform documents and
 * reports stable error codes so a repo can wire it into CI.
 */
export const VIEW_CONTRACT = "app-views/v1";
export const BUNDLE_CODES = Object.freeze([
  "bundle/app-md-missing",
  "bundle/frontmatter",
  "bundle/field-missing",
  "bundle/app-invalid",
  "bundle/version-invalid",
  "bundle/asset-missing",
  "bundle/workflow-missing",
  "views/invalid-json",
  "views/contract",
  "views/app-mismatch",
  "views/empty",
  "views/field-missing",
  "views/duplicate-id",
  "workflow/frontmatter",
  "workflow/field-missing",
  "workflow/empty",
  "rubric/empty",
]);

const APP_SLUG = /^[a-z0-9][a-z0-9-]{0,63}$/;
const SEMVER = /^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/;

const strip = (value) => {
  const text = value.trim();
  if (text.length >= 2 && ((text.startsWith('"') && text.endsWith('"')) || (text.startsWith("'") && text.endsWith("'")))) {
    return text.slice(1, -1);
  }
  return text;
};

/** Top-level scalars plus the `listing:` block (icon, screenshots). */
export function parseFrontmatter(text) {
  const match = /^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/.exec(text);
  if (!match) return null;
  const values = {};
  const listing = {};
  let block = null;
  let listKey = null;
  for (const raw of match[1].split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;
    const indent = raw.length - raw.trimStart().length;
    if (indent === 0) {
      const at = line.indexOf(":");
      if (at < 0) continue;
      const key = line.slice(0, at).trim();
      const value = line.slice(at + 1).trim();
      if (value) {
        values[key] = strip(value);
        block = null;
      } else {
        block = key;
      }
      listKey = null;
      continue;
    }
    if (block !== "listing") continue;
    if (line.startsWith("- ")) {
      if (listKey) (listing[listKey] ??= []).push(strip(line.slice(2)));
      continue;
    }
    const at = line.indexOf(":");
    if (at < 0) continue;
    const key = line.slice(0, at).trim();
    const value = line.slice(at + 1).trim();
    if (value === "[]") {
      listing[key] = [];
      listKey = null;
    } else if (value) {
      listing[key] = strip(value);
      listKey = null;
    } else {
      listKey = key;
      listing[key] = [];
    }
  }
  return { values, listing, body: text.slice(match[0].length) };
}

/**
 * @param {Map<string,string>|Record<string,string>} input bundle files keyed
 *        by path relative to the app root (`app/app.md`, `app/views/…`).
 * @returns {{ ok: boolean, errors: { code: string, path: string, message: string }[] }}
 */
export function validateBundle(input) {
  const files = input instanceof Map ? input : new Map(Object.entries(input ?? {}));
  const errors = [];
  const add = (code, path, message) => errors.push({ code, path, message });

  const manifest = files.get("app/app.md");
  let app = null;
  if (typeof manifest !== "string") {
    add("bundle/app-md-missing", "app/app.md", "an app bundle needs app/app.md");
  } else {
    const front = parseFrontmatter(manifest);
    if (!front) {
      add("bundle/frontmatter", "app/app.md", "app.md must start with a --- frontmatter block");
    } else {
      for (const key of ["app", "title", "version", "summary"]) {
        if (!front.values[key]) add("bundle/field-missing", `app/app.md:${key}`, `frontmatter needs ${key}`);
      }
      app = front.values.app ?? null;
      if (app && !APP_SLUG.test(app)) add("bundle/app-invalid", "app/app.md:app", "app must be a lowercase slug");
      if (front.values.version && !SEMVER.test(front.values.version)) {
        add("bundle/version-invalid", "app/app.md:version", "version must look like 1.2.3");
      }
      const shots = Array.isArray(front.listing.screenshots) ? front.listing.screenshots : [];
      for (const asset of [front.listing.icon, ...shots].filter((value) => typeof value === "string" && value)) {
        const path = asset.startsWith("app/") ? asset : `app/${asset}`;
        if (!files.has(path)) add("bundle/asset-missing", path, `${asset} is declared in listing but missing`);
      }
    }
  }

  const viewIds = new Set();
  for (const [path, text] of files) {
    if (!/^app\/views\/[^/]+\.json$/.test(path) || typeof text !== "string") continue;
    let parsed;
    try {
      parsed = JSON.parse(text);
    } catch {
      add("views/invalid-json", path, "views file is not JSON");
      continue;
    }
    if (parsed?.contract !== VIEW_CONTRACT) add("views/contract", path, `contract must be ${VIEW_CONTRACT}`);
    if (app && parsed?.app && parsed.app !== app) add("views/app-mismatch", path, `app ${parsed.app} does not match ${app}`);
    if (!Array.isArray(parsed?.views) || parsed.views.length === 0) {
      add("views/empty", path, "views must be a non-empty array");
      continue;
    }
    parsed.views.forEach((view, index) => {
      if (!view || typeof view !== "object" || Array.isArray(view)) {
        add("views/field-missing", `${path}#${index}`, "view must be an object");
        return;
      }
      for (const key of ["id", "title", "kind"]) {
        if (typeof view[key] !== "string" || view[key].length === 0) {
          add("views/field-missing", `${path}#${index}`, `view needs ${key}`);
        }
      }
      if (typeof view.id === "string" && view.id) {
        if (viewIds.has(view.id)) add("views/duplicate-id", `${path}#${index}`, `duplicate view id ${view.id}`);
        viewIds.add(view.id);
      }
    });
  }

  const workflows = [...files.keys()].filter((path) => /^app\/workflows\/[^/]+\.md$/.test(path));
  if (workflows.length === 0) add("bundle/workflow-missing", "app/workflows", "a bundle needs at least one workflow");
  for (const path of workflows) {
    const front = parseFrontmatter(files.get(path) ?? "");
    if (!front) {
      add("workflow/frontmatter", path, "workflow must start with a --- frontmatter block");
      continue;
    }
    for (const key of ["title", "goal"]) {
      if (!front.values[key]) add("workflow/field-missing", `${path}:${key}`, `workflow needs ${key}`);
    }
    if (!front.body.trim()) add("workflow/empty", path, "workflow has no body");
  }

  for (const [path, text] of files) {
    if (/^app\/rubrics\/[^/]+\.md$/.test(path) && typeof text === "string" && !text.trim()) {
      add("rubric/empty", path, "rubric is empty");
    }
  }

  return { ok: errors.length === 0, errors };
}
