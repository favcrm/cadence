import assert from "node:assert/strict";
import { test } from "node:test";
import { spawn } from "node:child_process";
import { mkdir, writeFile, rm } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { resolve } from "node:path";
const repo = fileURLToPath(new URL("../../", import.meta.url));
test("real Vite preview updates CSS over HMR, blocks APIs/files, and omits inherited VITE secrets", async () => {
  const name = `smoke-${process.pid}`,
    dir = resolve(repo, "app-previews", name),
    url = "http://127.0.0.1:3198";
  await mkdir(dir);
  await writeFile(
    resolve(dir, "index.html"),
    '<script type="module" src="/main.js"></script>',
  );
  await writeFile(
    resolve(dir, "main.js"),
    'import "./style.css"; console.log(import.meta.env);',
  );
  await writeFile(resolve(dir, "style.css"), "body{color:red}");
  const child = spawn(
    process.execPath,
    [resolve(repo, "scripts/app-dev.mjs"), name, "--port", "3198"],
    {
      cwd: repo,
      env: { ...process.env, VITE_PRIVATE_SENTINEL: "must-not-reach-client" },
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  const exited = new Promise((resolve) => child.once("exit", resolve));
  let socket;
  try {
    await new Promise((resolve, reject) => {
      let output = "";
      child.stdout.on("data", (chunk) => {
        output += chunk;
        if (output.includes(`App development: ${name}`)) resolve();
      });
      child.once("exit", (code) =>
        reject(new Error(`Owned preview exited before listening: ${code}`)),
      );
      child.once("error", reject);
      setTimeout(
        () => reject(new Error("Owned preview startup timeout")),
        8000,
      ).unref();
    });
    const module = await (await fetch(`${url}/main.js`)).text();
    assert.ok(!module.includes("must-not-reach-client"));
    assert.ok(!module.includes("VITE_PRIVATE_SENTINEL"));
    assert.equal((await fetch(`${url}/api/meta`)).status, 403);
    assert.equal((await fetch(`${url}/__platform/session`)).status, 403);
    assert.equal((await fetch(`${url}/@fs${repo}/Cargo.toml`)).status, 403);
    assert.equal((await fetch(`${url}/@fs${repo}/design/tokens.css`)).status, 200);
    assert.equal((await fetch(`${url}/@fs${repo}/design/kit.css`)).status, 200);
    assert.equal((await fetch(`${url}/@fs${repo}/ui/src/styles.css`)).status, 403);
    const client = await (await fetch(`${url}/@vite/client`)).text();
    const token = client.match(/const wsToken = "([^"]+)"/)[1];
    socket = new WebSocket(`ws://127.0.0.1:3198/?token=${token}`, "vite-hmr");
    await new Promise((resolve, reject) => {
      socket.onopen = resolve;
      socket.onerror = reject;
      setTimeout(
        () => reject(new Error("HMR connection timeout")),
        5000,
      ).unref();
    });
    await fetch(`${url}/style.css`);
    const update = new Promise((resolve, reject) => {
      socket.onmessage = (e) => {
        const message = JSON.parse(e.data);
        if (message.type === "update") resolve(message);
      };
      setTimeout(
        () => reject(new Error("No HMR update observed")),
        5000,
      ).unref();
    });
    await writeFile(resolve(dir, "style.css"), "body{color:green}");
    const message = await update;
    assert.ok(
      message.updates.some((u) => u.path === "/style.css"),
      "CSS HMR update observed",
    );
    assert.ok(
      (await (await fetch(`${url}/style.css`)).text()).includes("green"),
    );
  } finally {
    socket?.close();
    child.kill("SIGTERM");
    await exited;
    await rm(dir, { recursive: true });
  }
});
