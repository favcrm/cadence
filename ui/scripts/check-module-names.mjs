import { readdirSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

// Extensionless imports must select the same module on Linux and macOS.
export function moduleNameCollisions(paths) {
  const modules = new Map();
  for (const path of paths) {
    if (!/\.(?:ts|tsx|js|jsx)$/.test(path)) continue;
    const key = path.replace(/\.(?:ts|tsx|js|jsx)$/, "").toLowerCase();
    const group = modules.get(key) ?? [];
    group.push(path);
    modules.set(key, group);
  }
  return [...modules.values()].filter((group) => group.length > 1);
}

function sourcePaths(root, directory = root) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    return entry.isDirectory() ? sourcePaths(root, path) : [relative(root, path)];
  });
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const root = fileURLToPath(new URL("../src", import.meta.url));
  const collisions = moduleNameCollisions(sourcePaths(root));
  if (collisions.length) {
    console.error("Case-insensitive module-name collisions:");
    for (const group of collisions) console.error(group.join(" / "));
    process.exitCode = 1;
  }
}
