import { execFileSync } from "node:child_process";
import { createServer } from "node:http";
import { existsSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const port = Number(process.env.CADENCE_APP_PORT ?? 3186);
if (!Number.isInteger(port) || port < 3110 || port > 3199) {
  console.error("dev-preview: CADENCE_APP_PORT must be an integer in 3110..3199");
  process.exit(2);
}

const entry = path.join(root, "app/screens/main/client.js");
if (!existsSync(entry)) {
  execFileSync("bun", [path.join(root, "scripts/build.mjs")], { cwd: root, stdio: "inherit" });
}

const media = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".svg": "image/svg+xml",
};

const server = createServer((request, response) => {
  const url = new URL(request.url ?? "/", `http://127.0.0.1:${port}`);
  const requested = url.pathname === "/" ? "/preview.html" : decodeURIComponent(url.pathname);
  const file = path.resolve(root, `.${requested}`);
  if (!file.startsWith(`${root}${path.sep}`) || !existsSync(file) || !statSync(file).isFile()) {
    response.writeHead(404, { "content-type": "text/plain; charset=utf-8" });
    response.end("not found");
    return;
  }
  response.writeHead(200, {
    "content-type": media[path.extname(file)] ?? "application/octet-stream",
    "cache-control": "no-store",
  });
  response.end(readFileSync(file));
});

server.listen(port, "127.0.0.1", () => {
  console.log(`app preview: http://127.0.0.1:${port}/ (surface only; use Cadence for the live host)`);
});

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => server.close(() => process.exit(0)));
}
