#!/usr/bin/env node
// Run one app preview inside a caller-supplied local AgenticOS runtime image.
//
// This is a local execution plane, not a control plane: the checkout is the
// only mount, the published port is loopback-only, and no Cadence state, PM,
// daemon socket, tracker or provider credential is passed. One container per
// app/worktree/port; two agents on different ports get distinct names.

import { createHash } from "node:crypto";
import { existsSync, realpathSync } from "node:fs";
import { createServer } from "node:net";
import { execFile as defaultExecFile, spawn as defaultSpawn } from "node:child_process";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const APP = /^[a-z][a-z0-9-]{0,63}$/;
const PORT_MIN = 3110;
const PORT_MAX = 3199;

function fail(message) {
  throw new Error(message);
}

export function parseContainerOptions(argv, repo = REPO) {
  const options = {
    app: "social-content",
    port: 3186,
    image: null,
    repo: resolve(repo),
    docker: "docker",
  };
  for (let i = 0; i < argv.length; i += 2) {
    const key = argv[i];
    const value = argv[i + 1];
    if (!value) fail(`Missing value for ${key}`);
    if (key === "--app") options.app = value;
    else if (key === "--port") options.port = Number(value);
    else if (key === "--image") options.image = value;
    else if (key === "--repo") options.repo = resolve(value);
    else if (key === "--docker") options.docker = value;
    else fail(`Unsupported option ${key}.`);
  }
  if (!APP.test(options.app)) fail("Use an app preview name, not a path.");
  if (
    !Number.isInteger(options.port) ||
    options.port < PORT_MIN ||
    options.port > PORT_MAX
  )
    fail(`Port must be ${PORT_MIN}-${PORT_MAX}.`);
  if (!options.image || options.image.startsWith("-") || /\s/.test(options.image))
    fail("Pass a non-empty local AgenticOS runtime image with --image.");
  const previews = resolve(options.repo, "app-previews");
  const appDir = resolve(previews, options.app);
  if (relative(previews, appDir) === ".." || relative(previews, appDir).startsWith(`..${sep}`) || appDir === previews)
    fail("Preview must be inside app-previews.");
  if (!existsSync(join(appDir, "index.html")))
    fail("App preview needs index.html.");
  return options;
}

export function containerName(options) {
  const digest = createHash("sha256")
    .update(`${realpathSync(options.repo)}:${options.port}`)
    .digest("hex")
    .slice(0, 8);
  return `cadence-app-${options.app}-${digest}`;
}

export function dockerArgs(options) {
  const uid = process.getuid?.();
  const gid = process.getgid?.();
  if (uid === undefined || gid === undefined)
    fail("Container preview requires a POSIX host.");
  // Create and start are separate commands: a signal between them removes the
  // created container before it can start, whereas `docker run` would start it
  // after that cleanup.
  return [
    "create",
    "--pull",
    "never",
    "--name",
    containerName(options),
    "--label",
    "cadence.app.preview=1",
    "-p",
    `127.0.0.1:${options.port}:${options.port}`,
    "--user",
    `${uid}:${gid}`,
    "-v",
    `${realpathSync(options.repo)}:/work`,
    "-w",
    "/work",
    "-e",
    "HOME=/tmp",
    "-e",
    "CADENCE_STATE_DIR=/tmp/cadence-state",
    "-e",
    "CADENCE_PM_DIR=/tmp/cadence-pm",
    options.image,
    "node",
    "scripts/app-dev.mjs",
    options.app,
    "--host",
    "0.0.0.0",
    "--port",
    String(options.port),
    "--container",
    "true",
  ];
}

export function dockerStartArgs(containerId) {
  return ["start", "--attach", containerId];
}

export function probePort(port) {
  return new Promise((resolvePort, reject) => {
    const server = createServer();
    server.once("error", (error) => {
      reject(
        error.code === "EADDRINUSE"
          ? new Error(`Port ${port} is already in use.`)
          : error,
      );
    });
    server.once("listening", () => server.close(() => resolvePort()));
    server.listen(port, "127.0.0.1");
  });
}

export function removeContainer(options, execFile = defaultExecFile, ref = null) {
  return new Promise((resolveRemove) => {
    execFile(options.docker, ["rm", "-f", ref ?? containerName(options)], () =>
      resolveRemove(true),
    );
  });
}

export async function runContainer(options, deps = {}) {
  const spawn = deps.spawn ?? defaultSpawn;
  const execFile = deps.execFile ?? defaultExecFile;
  const checkPort = deps.probePort ?? probePort;
  let containerId = null;
  let stopping = false;
  const stop = () => {
    if (stopping) return;
    stopping = true;
    const id = containerId;
    if (id) {
      containerId = null;
      removeContainer(options, execFile, id).catch(() => {});
    }
  };
  // Install before any Docker call. A signal during preflight, create or start
  // must remove the container once it exists and never start a new one.
  for (const signal of ["SIGINT", "SIGTERM"]) process.once(signal, stop);
  try {
    const docker = (args, message) =>
      new Promise((resolveDocker, reject) => {
        execFile(options.docker, args, (error) => {
          if (error) reject(new Error(message));
          else resolveDocker();
        });
      });
    if (deps.checkDocker !== false) {
      await docker(
        ["version", "--format", "{{.Server.Version}}"],
        "Docker is not available.",
      );
      await docker(
        ["image", "inspect", "--format", "{{.Id}}", options.image],
        `Image ${options.image} is not available locally.`,
      );
    }
    await checkPort(options.port);
    await removeContainer(options, execFile);
    if (stopping) return { code: 143, signal: "SIGTERM" };
    containerId = await new Promise((resolveCreate, reject) => {
      execFile(options.docker, dockerArgs(options), (error, stdout) => {
        if (error) reject(new Error(`Docker create failed: ${error.message}`));
        else resolveCreate(String(stdout).trim());
      });
    });
    if (!containerId) throw new Error("Docker create returned no container id.");
    if (stopping) {
      if (containerId) {
        await removeContainer(options, execFile, containerId);
        containerId = null;
      }
      return { code: 143, signal: "SIGTERM" };
    }
    console.log(
      `App preview container: ${containerName(options)} -> http://127.0.0.1:${options.port}/`,
    );
    const child = spawn(options.docker, dockerStartArgs(containerId), {
      stdio: "inherit",
    });
    return await new Promise((resolveExit, reject) => {
      child.once("error", reject);
      child.once("exit", (code, signal) => resolveExit({ code, signal }));
    });
  } finally {
    for (const signal of ["SIGINT", "SIGTERM"]) process.removeListener(signal, stop);
    if (containerId) await removeContainer(options, execFile, containerId);
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const options = parseContainerOptions(process.argv.slice(2));
    const result = await runContainer(options);
    process.exitCode = result.code ?? 1;
  } catch (error) {
    console.error(`container preview: ${error.message}`);
    process.exitCode = 1;
  }
}
