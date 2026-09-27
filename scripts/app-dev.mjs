#!/usr/bin/env node
import { createRequire } from "node:module";
import { realpathSync, existsSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import { resolve, relative, sep } from "node:path";
import { execFileSync } from "node:child_process";
import { devOptions } from "./app-dev/options.mjs";

const repo = fileURLToPath(new URL("../", import.meta.url));
const options = devOptions(process.argv.slice(2));
const previews = realpathSync(resolve(repo, "app-previews"));
const root = realpathSync(resolve(previews, options.app));
if (
  relative(previews, root) === ".." ||
  relative(previews, root).startsWith(`..${sep}`) ||
  root === previews
)
  throw new Error("Preview must be inside app-previews.");
if (!existsSync(resolve(root, "index.html")))
  throw new Error("App preview needs index.html.");
const require = createRequire(resolve(repo, "ui/package.json"));
let vite;
try {
  vite = await import(pathToFileURL(require.resolve("vite")).href);
} catch {
  throw new Error(
    "Install the existing UI tools first: pnpm -C ui install --frozen-lockfile",
  );
}
const react = (
  await import(pathToFileURL(require.resolve("@vitejs/plugin-react")).href)
).default;
let revision = "unknown";
try {
  revision = execFileSync("git", ["rev-parse", "--short", "HEAD"], {
    cwd: repo,
    encoding: "utf8",
  }).trim();
} catch {
  /* source archive */
}
const server = await vite.createServer({
  root,
  configFile: false,
  envDir: false,
  envPrefix: "APP_DEV_PUBLIC_",
  plugins: [
    react(),
    {
      name: "fixtures-only",
      configureServer(server) {
        server.middlewares.use((req, res, next) => {
          if (/^\/(api|__platform)(\/|\?|$)/.test(req.url || "")) {
            res.statusCode = 403;
            res.end("Development fixtures only; live API is unavailable.");
            return;
          }
          next();
        });
      },
    },
  ],
  resolve: {
    alias: {
      react: resolve(repo, "ui/node_modules/react"),
      "react-dom": resolve(repo, "ui/node_modules/react-dom"),
      "@fontsource": resolve(repo, "ui/node_modules/@fontsource"),
    },
  },
  define: { __APP_DEV_REVISION__: JSON.stringify(revision) },
  server: {
    host: options.host,
    port: options.port,
    strictPort: true,
    allowedHosts: options.allowedHosts,
    fs: { strict: true, allow: [root, resolve(repo, "ui/node_modules")] },
    hmr: { port: options.port },
  },
});
await server.listen();
console.log(
  `App development: ${options.app} · FIXTURES ONLY · revision ${revision}`,
);
server.printUrls();
for (const host of options.allowedHosts.filter((host) => host !== options.host))
  console.log(`Private preview: http://${host}:${options.port}/`);
for (const signal of ["SIGINT", "SIGTERM"])
  process.once(signal, async () => {
    await server.close();
    process.exit(0);
  });
